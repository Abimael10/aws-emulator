// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation-declared `AWS::Cognito::IdentityPoolPrincipalTag` leaves behind in
//! Amazon Cognito Identity, and what the pool it maps then does when it vends credentials.
//!
//! RULE under test: **a CloudFormation resource type that is the template form of one of its
//! owning service's calls must APPLY that call, with every property the declaration states,
//! into the owning service's own state before the resource is reported created — and apply it
//! again when the declaration changes** — so the owning service's own read of that state
//! reports what the template declared, and everything that reads that state behaves as if a
//! client had made the call itself. For this type the call is `SetPrincipalTagAttributeMap`,
//! addressed by the `(IdentityPoolId, IdentityProviderName)` pair the declaration names, so
//! `GetPrincipalTagAttributeMap` on that pair is SERVED with the declared `PrincipalTags` and
//! `UseDefaults` and the echoed pair; a pool no such resource names carries no mapping and is
//! answered with the modelled `ResourceNotFoundException` instead, on the client-error status
//! this `awsJson1_1` service answers every modelled client error with (its own
//! `scaffolded::CLIENT_ERROR_STATUS`, the rule its `wire_error_envelope` suite states).
//! Downstream of
//! that state, the mapping is what makes the pool's exchange a TAGGED
//! `sts:AssumeRoleWithWebIdentity`, so the role the pool attaches needs `sts:TagSession`
//! beside the assume ("To pass session tags, the principal must have the `sts:TagSession`
//! permission", `AssumeRole$Tags`) — a role granting the assume alone cannot be assumed with
//! tags, which is the refusal the modelled `InvalidIdentityPoolConfigurationException`
//! states, while an identically-trusted pool with no mapping vends.
//!
//! Only a cross-service suite can pin this: CloudFormation makes the create, update and
//! delete calls, Amazon Cognito user pools serves the pool, the app client and the sign-in
//! that mints the token the mapped claim is read out of, Amazon Cognito Identity holds the
//! identity pools, their roles and the attribute map, IAM holds the role and its trust policy
//! and STS resolves the session the exchange vends — five crates over ONE engine, as
//! `core/router/src/lib.rs` mounts them.
//!
//! The rows of the first test differ along what the rule generalises over: how many pairs the
//! declared map carries (one, three), what each pair maps (a developer-defined claim, a
//! standard one), the spelling the template gives the `UseDefaults` flag (a JSON boolean, the
//! text a template writes one as, no flag at all), the default mappings asked for INSTEAD of a
//! custom map, a pool the template declares no mapping for at all — and then, over the same
//! stack updated in place, a map that grows, one that shrinks and one that replaces the
//! default mappings with a custom one. Every expected value is computed from what the row
//! declares and from the ids the owning services minted in the same run.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread. The wire harness is the sibling federation suite's
//! (`cloudformation_cognito_federation_estate.rs`): the same three services over one assembled
//! router, so the encoder, the signer, the settle loop and the deploy and update it wraps are
//! called from there rather than copied.

use std::collections::BTreeMap;
use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Map, Value};
use service_com_amazonaws_cognitoidentity_awscognitoidentityservice::scaffolded::CLIENT_ERROR_STATUS;
use service_com_amazonaws_cognitoidentity_awscognitoidentityservice::state::DEFAULT_PRINCIPAL_TAGS;

use crate::cloudformation_cognito_federation_estate::{
    is_minted_identity_pool_id, Wire, BOOT_STACK_SIZE, COGNITO_IDENTITY, COGNITO_IDP,
    SETUP_KEY_ID,
};
use crate::cloudformation_provisions_cognito_user_pools::output;
// The assumed-role ARN a session an identity pool vended speaks for, from the sibling that
// states it once for every suite of this target that reads one.
use crate::cognito_group_role_mapping::session_arn;
use crate::query_wire::element;

/// The Region every call below is signed for, and so the Region every resource lives in.
const REGION: &str = "us-east-1";

/// The API versions the awsQuery services this suite calls directly declare.
const IAM_API_VERSION: &str = "2010-05-08";
const STS_API_VERSION: &str = "2011-06-15";

/// The separator CloudFormation joins the parts of a COMPOSITE physical resource id with,
/// which is what a `Ref` of this type publishes.
const PAIR_SEPARATOR: char = '|';

/// The Cognito Identity principal a role's trust policy names to admit an identity pool's
/// users, as `AssumeRoleWithWebIdentityRequest$RoleArn` documents it.
const COGNITO_PRINCIPAL: &str = "cognito-identity.amazonaws.com";

/// The assume a pool's exchange makes, and the permission that attaches its session tags.
const ASSUME_ACTION: &str = "sts:AssumeRoleWithWebIdentity";
const TAG_SESSION_ACTION: &str = "sts:TagSession";

/// The modelled error a vend whose assume STS would not serve is refused with, and how AWS
/// words it.
const INVALID_POOL_CONFIGURATION_CODE: &str = "InvalidIdentityPoolConfigurationException";
const INVALID_POOL_CONFIGURATION: &str =
    "Invalid identity pool configuration. Check assigned IAM roles for this pool.";

/// The developer-defined user-pool attribute the mapped tag is taken from: declared in the
/// pool's `Schema` under its bare name, stored and claimed under the `custom:` prefix every
/// developer-defined attribute carries (`AttributeType$Name`).
const TENANT_SCHEMA_NAME: &str = "tenant";
const TENANT_CLAIM: &str = "custom:tenant";

/// The principal tag the mapped pool maps that claim onto, and the value the signed-in user
/// carries in it.
const TENANT_TAG: &str = "tenant";
const TENANT: &str = "acme";

/// The error code of an awsJson answer, whose `__type` may be namespace-qualified.
fn code_of(body: &Value) -> String {
    body["__type"].as_str().unwrap_or_default().to_string()
}

/// The message of an awsJson refusal, in either spelling the envelope gives the member.
fn message_of(body: &Value) -> String {
    for member in ["message", "Message"] {
        if let Some(text) = body[member].as_str() {
            return text.to_string();
        }
    }
    String::new()
}

/// The `PrincipalTags` of a `GetPrincipalTagAttributeMap` answer, as the comparison takes it.
fn reported_tags(body: &Value) -> BTreeMap<String, String> {
    body["PrincipalTags"]
        .as_object()
        .map(|tags| {
            tags.iter()
                .map(|(key, value)| (key.clone(), value.as_str().unwrap_or_default().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// The pairs a row expects, as the comparison takes them.
fn expected_tags(tags: &[(&str, &str)]) -> BTreeMap<String, String> {
    tags.iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// `GetPrincipalTagAttributeMap` for one pair, signed as a client holding the account's own
/// credential calls it: the status and the parsed body.
fn mapping_of(wire: &Wire<'_>, pool_id: &str, provider: &str) -> (u16, Value) {
    let answered = wire.rpc(
        COGNITO_IDENTITY,
        true,
        REGION,
        "GetPrincipalTagAttributeMap",
        &json!({"IdentityPoolId": pool_id, "IdentityProviderName": provider}),
    );
    (answered.status, answered.json())
}

/// What the owning service must report for one row's pair.
enum Expect {
    /// The mapping is in force: `UseDefaults` as reported, and the pairs the read carries.
    Mapped {
        use_defaults: bool,
        tags: Vec<(&'static str, &'static str)>,
    },
    /// No mapping at all for the pair, which is the modelled refusal.
    Unmapped,
}

/// Which phase of the deployment a reading is taken in: what the stack was CREATED with, and
/// what an in-place update then declared.
///
/// One value carries both the label a case is named by and which of a row's two declarations
/// and two expectations are in force, so a reading cannot be labelled one phase while held to
/// the other's expectation.
#[derive(Clone, Copy)]
enum Phase {
    Create,
    Update,
}

impl Phase {
    /// The label every case of this phase carries.
    fn label(self) -> &'static str {
        match self {
            Self::Create => "the create",
            Self::Update => "the update",
        }
    }

    /// The mapping members `row`'s template states in this phase.
    fn declared(self, row: &Row) -> &Value {
        match self {
            Self::Create => &row.declared,
            Self::Update => &row.redeclared,
        }
    }

    /// What the owning service must report for `row` in this phase.
    fn expected(self, row: &Row) -> &Expect {
        match self {
            Self::Create => &row.expected,
            Self::Update => &row.expected_after,
        }
    }
}

/// One row: the identity pool it is about, the mapping the template declares on it — before
/// and after the in-place update — and what the owning service must report in each phase.
struct Row {
    case: &'static str,
    /// The logical id of the `AWS::Cognito::IdentityPool` the row's mapping is declared on,
    /// which is also the `IdentityPoolName` the template gives it and the output key its
    /// published id comes back under.
    pool: &'static str,
    /// The logical id of the `AWS::Cognito::IdentityPoolPrincipalTag`, or `None` for a pool
    /// the template declares no mapping for.
    mapping: Option<&'static str>,
    /// The mapping members the template states beside the pair, at the create and after the
    /// in-place update.
    declared: Value,
    redeclared: Value,
    expected: Expect,
    expected_after: Expect,
}

/// The rows: every form of declared mapping, a pool with none, and a changed declaration for
/// each that grows, shrinks or replaces what was in force.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one pair, the flag written as a JSON boolean",
            pool: "SingleTagPool",
            mapping: Some("SingleTagMapping"),
            declared: json!({"UseDefaults": false, "PrincipalTags": {TENANT_TAG: TENANT_CLAIM}}),
            // The same mapping GROWN by a second pair.
            redeclared: json!({
                "UseDefaults": false,
                "PrincipalTags": {TENANT_TAG: TENANT_CLAIM, "seat": "custom:seat"},
            }),
            expected: Expect::Mapped {
                use_defaults: false,
                tags: vec![(TENANT_TAG, TENANT_CLAIM)],
            },
            expected_after: Expect::Mapped {
                use_defaults: false,
                tags: vec![("seat", "custom:seat"), (TENANT_TAG, TENANT_CLAIM)],
            },
        },
        Row {
            case: "three pairs, one of them a standard claim, the flag written as text",
            pool: "ManyTagsPool",
            mapping: Some("ManyTagsMapping"),
            declared: json!({
                "UseDefaults": "false",
                "PrincipalTags": {
                    TENANT_TAG: TENANT_CLAIM,
                    "dept": "custom:dept",
                    "who": "sub",
                },
            }),
            // The same mapping SHRUNK to one pair, stating no flag at all.
            redeclared: json!({"PrincipalTags": {"who": "sub"}}),
            expected: Expect::Mapped {
                use_defaults: false,
                tags: vec![
                    ("dept", "custom:dept"),
                    (TENANT_TAG, TENANT_CLAIM),
                    ("who", "sub"),
                ],
            },
            expected_after: Expect::Mapped {
                use_defaults: false,
                tags: vec![("who", "sub")],
            },
        },
        Row {
            case: "the default mappings asked for instead of a custom map",
            pool: "DefaultsPool",
            mapping: Some("DefaultsMapping"),
            declared: json!({"UseDefaults": true}),
            // The defaults REPLACED by a custom map.
            redeclared: json!({"UseDefaults": false, "PrincipalTags": {TENANT_TAG: TENANT_CLAIM}}),
            expected: Expect::Mapped {
                use_defaults: true,
                tags: DEFAULT_PRINCIPAL_TAGS.to_vec(),
            },
            expected_after: Expect::Mapped {
                use_defaults: false,
                tags: vec![(TENANT_TAG, TENANT_CLAIM)],
            },
        },
        Row {
            case: "a pool the template declares no mapping for",
            pool: "UnmappedPool",
            mapping: None,
            declared: json!({}),
            redeclared: json!({}),
            expected: Expect::Unmapped,
            expected_after: Expect::Unmapped,
        },
    ]
}

/// One identity pool federating the user pool's app client, which is what makes a mapping for
/// that provider one the pool's own configuration admits.
fn identity_pool(logical: &str) -> Value {
    json!({
        "Type": "AWS::Cognito::IdentityPool",
        "Properties": {
            "IdentityPoolName": logical,
            "AllowUnauthenticatedIdentities": false,
            "CognitoIdentityProviders": [{
                "ProviderName": {"Fn::GetAtt": ["UserPool", "ProviderName"]},
                "ClientId": {"Ref": "UserPoolClient"},
            }],
        },
    })
}

/// The template the table deploys in `phase`: the user pool and app client every identity
/// pool federates, one identity pool per row, the mapping each row declares — in that phase's
/// own spelling — and an output per resource, so every published handle is read off the wire.
fn table_template(rows: &[Row], phase: Phase) -> String {
    let mut resources = Map::new();
    resources.insert(
        "UserPool".to_string(),
        json!({
            "Type": "AWS::Cognito::UserPool",
            "Properties": {"UserPoolName": "ptag-mapping-estate"},
        }),
    );
    resources.insert(
        "UserPoolClient".to_string(),
        json!({
            "Type": "AWS::Cognito::UserPoolClient",
            "Properties": {"UserPoolId": {"Ref": "UserPool"}, "ClientName": "ptag-mapping-app"},
        }),
    );
    let mut outputs = Map::new();
    outputs.insert(
        "ProviderName".to_string(),
        json!({"Value": {"Fn::GetAtt": ["UserPool", "ProviderName"]}}),
    );
    outputs.insert(
        "UserPoolId".to_string(),
        json!({"Value": {"Ref": "UserPool"}}),
    );
    for row in rows {
        resources.insert(row.pool.to_string(), identity_pool(row.pool));
        outputs.insert(
            row.pool.to_string(),
            json!({"Value": {"Ref": row.pool}}),
        );
        let Some(logical) = row.mapping else {
            continue;
        };
        let mut properties = phase.declared(row).clone();
        let members = properties
            .as_object_mut()
            .expect("the declared mapping members are an object");
        members.insert("IdentityPoolId".to_string(), json!({"Ref": row.pool}));
        members.insert(
            "IdentityProviderName".to_string(),
            json!({"Fn::GetAtt": ["UserPool", "ProviderName"]}),
        );
        resources.insert(
            logical.to_string(),
            json!({
                "Type": "AWS::Cognito::IdentityPoolPrincipalTag",
                "Properties": properties,
            }),
        );
        outputs.insert(
            logical.to_string(),
            json!({"Value": {"Ref": logical}}),
        );
    }
    json!({"Resources": resources, "Outputs": outputs}).to_string()
}

/// Hold every row to the rule in one phase of the deployment: the published pair is the two
/// identifiers the owning service's own call takes, and the state behind it is what the
/// template declared.
fn check(
    wire: &Wire<'_>,
    rows: &[Row],
    outputs: &BTreeMap<String, String>,
    provider: &str,
    phase: Phase,
) {
    for row in rows {
        let case = format!("{}: {}", phase.label(), row.case);
        let pool_id = output(outputs, row.pool, &case);
        assert!(
            is_minted_identity_pool_id(&pool_id, REGION),
            "case {case:?}: expected the published pool handle to be the REGION:GUID the \
             owning service minted, got {pool_id:?}"
        );
        if let Some(logical) = row.mapping {
            // The resource's own identity is the pair the mapping is addressed by, so a
            // client holding nothing but the published value recovers both arguments of the
            // owning service's call from it.
            let published = output(outputs, logical, &case);
            let pair = format!("{pool_id}{PAIR_SEPARATOR}{provider}");
            assert_eq!(
                published, pair,
                "case {case:?}: expected the Ref of the mapping to be the pair it is \
                 addressed by {pair:?}, got {published:?}"
            );
        }
        let expected = phase.expected(row);
        let (status, body) = mapping_of(wire, &pool_id, provider);
        match expected {
            Expect::Mapped { use_defaults, tags } => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the declared mapping to be served, got {status} \
                     with {body}"
                );
                assert_eq!(
                    body["IdentityPoolId"].as_str(),
                    Some(pool_id.as_str()),
                    "case {case:?}: expected the answer to echo the pool {pool_id:?}, got \
                     {body}"
                );
                assert_eq!(
                    body["IdentityProviderName"].as_str(),
                    Some(provider),
                    "case {case:?}: expected the answer to echo the provider {provider:?}, \
                     got {body}"
                );
                assert_eq!(
                    body["UseDefaults"].as_bool(),
                    Some(*use_defaults),
                    "case {case:?}: expected UseDefaults {use_defaults}, got {body}"
                );
                let reported = reported_tags(&body);
                let wanted = expected_tags(tags);
                assert_eq!(
                    reported, wanted,
                    "case {case:?}: expected the declared mapping {wanted:?} to be the one \
                     the owning service reports, got {reported:?}"
                );
            }
            Expect::Unmapped => {
                assert_eq!(
                    status, CLIENT_ERROR_STATUS,
                    "case {case:?}: expected a pool carrying no mapping to be refused \
                     {CLIENT_ERROR_STATUS}, got {status} with {body}"
                );
                let code = code_of(&body);
                assert!(
                    code.ends_with("ResourceNotFoundException"),
                    "case {case:?}: expected ResourceNotFoundException, got {code:?} in {body}"
                );
                let stated = message_of(&body);
                let wanted = format!("No Principal Tags configured for Provider {provider}");
                assert_eq!(
                    stated, wanted,
                    "case {case:?}: expected the refusal {wanted:?}, got {stated:?}"
                );
            }
        }
    }
}

/// RULE: the mapping a stack declares is the one the owning service reports, at the create
/// and again after an in-place update that changes the declaration — and a pool the stack
/// declares no mapping for carries none.
#[test]
fn a_declared_principal_tag_mapping_is_the_one_cognito_identity_reports() {
    std::thread::Builder::new()
        .name("cfn-cognito-principal-tag-mapping".to_string())
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

            let rows = rows();
            let stack = "cfn-cognito-principal-tag-mapping";
            let case = Phase::Create.label();
            let outputs = wire.deploy(
                REGION,
                stack,
                &table_template(&rows, Phase::Create),
                case,
            );

            // The provider every mapping is addressed by, computed from the published user-pool
            // handle and the Region rather than read out of an answer being checked — and the
            // attribute the template's own `Fn::GetAtt` resolved to, which has to be the same.
            let user_pool = output(&outputs, "UserPoolId", case);
            let provider = format!("cognito-idp.{REGION}.amazonaws.com/{user_pool}");
            assert_eq!(
                output(&outputs, "ProviderName", case),
                provider,
                "case {case:?}: expected the published provider name to be the one the user \
                 pool publishes for the handle it was created under"
            );

            check(&wire, &rows, &outputs, &provider, Phase::Create);

            let updated = wire.update_stack(
                REGION,
                stack,
                &table_template(&rows, Phase::Update),
                Phase::Update.label(),
            );
            assert_eq!(
                output(&updated, "UserPoolId", Phase::Update.label()),
                user_pool,
                "the in-place update must keep the estate it is updating"
            );
            check(&wire, &rows, &updated, &provider, Phase::Update);
        })
        .expect("spawn the principal-tag mapping thread")
        .join()
        .expect("a declared principal-tag mapping must be the one Cognito Identity reports");
}

/// The logical ids of the second test's estate: two pools trusted in exactly the same way,
/// one of which the stack declares a principal-tag mapping for.
const MAPPED_POOL: &str = "MappedPool";
const UNMAPPED_POOL: &str = "UnmappedPool";
const MAPPED_ROLE: &str = "MappedRole";
const UNMAPPED_ROLE: &str = "UnmappedRole";

/// The user the second test signs in, and the password it signs in with.
const USERNAME: &str = "tenant.user";
const PASSWORD: &str = "Generated#Pass42";

/// A trust policy admitting one identity pool's users: the `Federated` Cognito Identity
/// principal, the actions granted, and the `:aud` condition that restricts the assume to the
/// users of the pool `audience` resolves to.
fn trust_policy(audience: Value, actions: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Federated": COGNITO_PRINCIPAL},
            "Action": actions,
            "Condition": {"StringEquals": {format!("{COGNITO_PRINCIPAL}:aud"): audience}},
        }],
    })
}

/// The template the second test deploys: a user pool declaring the developer-defined
/// attribute the mapping reads, an app client of it, two identity pools federating that
/// client, one role per pool whose trust grants the assume ALONE, the attachment that binds
/// each, and a principal-tag mapping on one of the two pools.
fn vend_template() -> String {
    json!({
        "Resources": {
            "UserPool": {
                "Type": "AWS::Cognito::UserPool",
                "Properties": {
                    "UserPoolName": "ptag-vend-estate",
                    "Schema": [{
                        "Name": TENANT_SCHEMA_NAME,
                        "AttributeDataType": "String",
                        "Mutable": true,
                    }],
                },
            },
            "UserPoolClient": {
                "Type": "AWS::Cognito::UserPoolClient",
                "Properties": {
                    "UserPoolId": {"Ref": "UserPool"},
                    "ClientName": "ptag-vend-app",
                    "ExplicitAuthFlows": [
                        "ALLOW_ADMIN_USER_PASSWORD_AUTH",
                        "ALLOW_REFRESH_TOKEN_AUTH",
                    ],
                },
            },
            MAPPED_POOL: identity_pool(MAPPED_POOL),
            UNMAPPED_POOL: identity_pool(UNMAPPED_POOL),
            MAPPED_ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": trust_policy(
                        json!({"Ref": MAPPED_POOL}),
                        json!(ASSUME_ACTION),
                    ),
                },
            },
            UNMAPPED_ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": trust_policy(
                        json!({"Ref": UNMAPPED_POOL}),
                        json!(ASSUME_ACTION),
                    ),
                },
            },
            "MappedAttachment": {
                "Type": "AWS::Cognito::IdentityPoolRoleAttachment",
                "Properties": {
                    "IdentityPoolId": {"Ref": MAPPED_POOL},
                    "Roles": {"authenticated": {"Fn::GetAtt": [MAPPED_ROLE, "Arn"]}},
                },
            },
            "UnmappedAttachment": {
                "Type": "AWS::Cognito::IdentityPoolRoleAttachment",
                "Properties": {
                    "IdentityPoolId": {"Ref": UNMAPPED_POOL},
                    "Roles": {"authenticated": {"Fn::GetAtt": [UNMAPPED_ROLE, "Arn"]}},
                },
            },
            "PrincipalTag": {
                "Type": "AWS::Cognito::IdentityPoolPrincipalTag",
                "Properties": {
                    "IdentityPoolId": {"Ref": MAPPED_POOL},
                    "IdentityProviderName": {"Fn::GetAtt": ["UserPool", "ProviderName"]},
                    "PrincipalTags": {TENANT_TAG: TENANT_CLAIM},
                    "UseDefaults": false,
                },
            },
        },
        "Outputs": {
            "UserPoolId": {"Value": {"Ref": "UserPool"}},
            "ClientId": {"Value": {"Ref": "UserPoolClient"}},
            MAPPED_POOL: {"Value": {"Ref": MAPPED_POOL}},
            UNMAPPED_POOL: {"Value": {"Ref": UNMAPPED_POOL}},
            MAPPED_ROLE: {"Value": {"Ref": MAPPED_ROLE}},
            UNMAPPED_ROLE: {"Value": {"Ref": UNMAPPED_ROLE}},
        },
    })
    .to_string()
}

/// Exchange `logins` for the credentials of an identity of `pool_id`: the unsigned `GetId`
/// that mints the identity, then the unsigned vend, whose status and body are answered as
/// they came back.
fn exchange(wire: &Wire<'_>, pool_id: &str, logins: &Value, case: &str) -> (u16, Value) {
    let minted = wire.rpc_ok(
        COGNITO_IDENTITY,
        false,
        REGION,
        "GetId",
        &json!({"IdentityPoolId": pool_id, "Logins": logins}),
        case,
    );
    let identity_id = minted["IdentityId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected GetId to mint an IdentityId: {minted}"))
        .to_string();
    let answered = wire.rpc(
        COGNITO_IDENTITY,
        false,
        REGION,
        "GetCredentialsForIdentity",
        &json!({"IdentityId": identity_id, "Logins": logins}),
    );
    (answered.status, answered.json())
}

/// The role a vended credential speaks for, read from STS under that credential itself.
fn vended_role(wire: &Wire<'_>, body: &Value, case: &str) -> String {
    let access_key_id = body["Credentials"]["AccessKeyId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the vend to carry an AccessKeyId: {body}"))
        .to_string();
    let answered = wire.query(
        "sts",
        &access_key_id,
        REGION,
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
    element(&answered.text(), "Arn")
}

/// RULE, downstream of the same state: because the declared mapping is really in force, the
/// mapped pool's exchange is a TAGGED assume and needs `sts:TagSession` on the role it
/// attaches, while the identically-trusted pool the stack declared no mapping for vends.
#[test]
fn a_declared_mapping_makes_the_pools_vend_a_tagged_assume() {
    std::thread::Builder::new()
        .name("cfn-cognito-principal-tag-vend".to_string())
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

            let case = "a CloudFormation-deployed principal-tag mapping";
            let stack = "cfn-cognito-principal-tag-vend";
            let outputs = wire.deploy(REGION, stack, &vend_template(), case);
            let user_pool = output(&outputs, "UserPoolId", case);
            let client = output(&outputs, "ClientId", case);
            let mapped_pool = output(&outputs, MAPPED_POOL, case);
            let unmapped_pool = output(&outputs, UNMAPPED_POOL, case);
            let mapped_role = output(&outputs, MAPPED_ROLE, case);
            let unmapped_role = output(&outputs, UNMAPPED_ROLE, case);
            let provider = format!("cognito-idp.{REGION}.amazonaws.com/{user_pool}");

            // The premise: the mapping the stack declared is in force on the one pool, and
            // the other pool carries none.
            let (status, body) = mapping_of(&wire, &mapped_pool, &provider);
            assert_eq!(
                status, 200,
                "case {case:?}: expected the declared mapping to be served, got {status} with \
                 {body}"
            );
            assert_eq!(
                reported_tags(&body),
                expected_tags(&[(TENANT_TAG, TENANT_CLAIM)]),
                "case {case:?}: expected the declared mapping to be the one in force, got \
                 {body}"
            );
            let (status, body) = mapping_of(&wire, &unmapped_pool, &provider);
            assert_eq!(
                status, CLIENT_ERROR_STATUS,
                "case {case:?}: expected the pool with no declared mapping to carry none, got \
                 {status} with {body}"
            );
            assert!(
                code_of(&body).ends_with("ResourceNotFoundException"),
                "case {case:?}: expected ResourceNotFoundException for the unmapped pool, got \
                 {body}"
            );

            // The user whose token carries the claim the mapping reads.
            wire.rpc_ok(
                COGNITO_IDP,
                true,
                REGION,
                "AdminCreateUser",
                &json!({
                    "UserPoolId": &user_pool,
                    "Username": USERNAME,
                    "MessageAction": "SUPPRESS",
                    "UserAttributes": [{"Name": TENANT_CLAIM, "Value": TENANT}],
                }),
                case,
            );
            wire.rpc_ok(
                COGNITO_IDP,
                true,
                REGION,
                "AdminSetUserPassword",
                &json!({
                    "UserPoolId": &user_pool,
                    "Username": USERNAME,
                    "Password": PASSWORD,
                    "Permanent": true,
                }),
                case,
            );
            let signed_in = wire.rpc_ok(
                COGNITO_IDP,
                true,
                REGION,
                "AdminInitiateAuth",
                &json!({
                    "UserPoolId": &user_pool,
                    "ClientId": &client,
                    "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
                    "AuthParameters": {"USERNAME": USERNAME, "PASSWORD": PASSWORD},
                }),
                case,
            );
            let id_token = signed_in["AuthenticationResult"]["IdToken"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected the sign-in to mint an IdToken: {signed_in}")
                })
                .to_string();
            let logins = json!({&provider: id_token});

            // The control: the pool with no mapping performs an UNTAGGED assume, which the
            // role granting the assume alone serves.
            let control = "the pool the stack declared no mapping for";
            let (status, body) = exchange(&wire, &unmapped_pool, &logins, control);
            assert_eq!(
                status, 200,
                "case {control:?}: an untagged assume must be served, got {status} with {body}"
            );
            let arn = vended_role(&wire, &body, control);
            assert_eq!(
                arn,
                session_arn(&unmapped_role),
                "case {control:?}: expected a session of the role the stack attached, got {arn}"
            );

            // The mapped pool, trusted in exactly the same way: its exchange attaches the
            // session tag the declared mapping asks for, which the role does not permit.
            let refused = "the mapped pool against a role granting the assume alone";
            let (status, body) = exchange(&wire, &mapped_pool, &logins, refused);
            assert_eq!(
                status, CLIENT_ERROR_STATUS,
                "case {refused:?}: a tagged assume the role does not permit must be refused, \
                 got {status} with {body}"
            );
            let code = code_of(&body);
            assert!(
                code.ends_with(INVALID_POOL_CONFIGURATION_CODE),
                "case {refused:?}: expected {INVALID_POOL_CONFIGURATION_CODE}, got {code:?} in \
                 {body}"
            );
            assert_eq!(
                message_of(&body),
                INVALID_POOL_CONFIGURATION,
                "case {refused:?}: expected {INVALID_POOL_CONFIGURATION:?}, got {body}"
            );
            assert!(
                body["Credentials"].is_null(),
                "case {refused:?}: a refused assume vends nothing, got {body}"
            );

            // The repair: the same role, granting the session-tag permission beside the
            // assume. The very same exchange is then served, and speaks for that role.
            let repaired = trust_policy(
                Value::String(mapped_pool.clone()),
                json!([ASSUME_ACTION, TAG_SESSION_ACTION]),
            )
            .to_string();
            let answered = wire.query(
                "iam",
                SETUP_KEY_ID,
                REGION,
                "UpdateAssumeRolePolicy",
                IAM_API_VERSION,
                &[
                    ("RoleName", mapped_role.as_str()),
                    ("PolicyDocument", repaired.as_str()),
                ],
            );
            assert_eq!(
                answered.status,
                200,
                "case {case:?}: expected UpdateAssumeRolePolicy to answer 200, got {} with \
                 body {:?}",
                answered.status,
                answered.text()
            );
            let served = "the mapped pool once the role permits the session tag";
            let (status, body) = exchange(&wire, &mapped_pool, &logins, served);
            assert_eq!(
                status, 200,
                "case {served:?}: the tagged assume must be served, got {status} with {body}"
            );
            let arn = vended_role(&wire, &body, served);
            assert_eq!(
                arn,
                session_arn(&mapped_role),
                "case {served:?}: expected a session of the role the stack attached, got {arn}"
            );
        })
        .expect("spawn the principal-tag vend thread")
        .join()
        .expect("a declared mapping must make the pool's vend a tagged assume");
}
