// SPDX-License-Identifier: CC0-1.0

//! The IDENTITY half of the SDK seam a RUNNING handler calls another service through: the
//! roles and policies of the account it runs in, built the way that service's own protocol
//! binds them.
//!
//! WHAT IDENTITY A FUNCTION RUNS AS, and what that identity may do, are the two facts a
//! handler establishes about itself. The first is the token service's
//! ([`super::aws_sts`]); the second is this service's — a handler reads the role it runs as
//! (`GetRole`), the documents in force on it (`GetRolePolicy`, `ListRolePolicies`,
//! `ListAttachedRolePolicies`, `GetPolicy`, `GetPolicyVersion`) and the roles and policies
//! of the account around it, and an automation handler WRITES them: the role-and-policy
//! surface a function that provisions an identity for a workload drives. A handler that
//! cannot reach this service can neither report what it is allowed to do nor administer an
//! identity, and — because a call that cannot be made and a call the identity service
//! refused come back the same way — a role that grants too much is indistinguishable from
//! one that is correctly scoped.
//!
//! Most services a handler reaches bind their requests with an AWS JSON protocol, where the
//! request body IS the operation's input document, so one code path serves every operation
//! of every one of them ([`super::aws_api`]). This service does not: its model declares
//! `awsQuery` (`smithy_query(query="service_info")`), so a request to it is a form-encoded
//! `Action` with the input flattened into the parameters beside it, and the answer is an XML
//! document. That binding is not written out here: the request is built, signed, dispatched
//! and parsed by the CLIENT this crate holds (`aws_sdk_iam`), over the same in-process
//! callback transport every other call of this seam uses, so the wire details are the
//! client's and the answer is the emulated identity service's own.
//!
//! What a guest can and cannot do here:
//!
//! * The destination is ALWAYS this emulator: the client is given the callback's own
//!   transport and the one endpoint this service answers at, so no host name is resolved and
//!   no socket is opened.
//! * The call is signed as the identity the client was built with — the session of the
//!   function's execution role, or the credentials the handler passed that client
//!   ([`super::aws_api::AwsAccess::signing_as`]) — so the identity service judges it against
//!   the policies in force on that principal exactly as it judges any other signed request:
//!   an explicit `Deny` about the request refuses it whatever else allows it, and a principal
//!   with no grant for it is refused too, each with that service's own code, message and
//!   status.
//! * Only the operations in [`BOUND_OPERATIONS`] are bound, and only the input members each
//!   entry names. An operation or a member this seam does not bind is reported as a call that
//!   was NOT MADE, naming it, rather than sent with a value dropped — a dropped member would
//!   make a call that did something other than what the handler asked for.
//! * The answer's own status and document are what the identity service answered: a success
//!   is that operation's output members, and a refusal is its own error code, message and
//!   status, which the SDK the guest calls raises as `ClientError`
//!   ([`super::aws_sdk_guest`]).
//!
//! How one call's input is read, what the answer carried and how a refusal is handed back are
//! the same for every service reached through an SDK client, so they are [`super::sdk_call`]'s
//! rather than this module's.

use ::aws_sdk_iam::Client;
use ::serde_json::{Map, Value};
use ::smithy_server_runtime::EmbeddedEmulator;

use super::aws_api::{Answer, CallerIdentity, Refusal};
use super::sdk_call::{
    insert_flag, insert_number, insert_text, insert_timestamp, Input, Observed, PROVIDER_NAME,
};

/// The service a refusal of one of these calls names, as the AWS SDKs name it.
const SERVICE: &str = "IAM";

/// The Region an identity-service call is signed for, whatever Region the client the guest
/// built names: IAM is a global service reached at one endpoint, and its credential scope
/// names the Region that endpoint is served from
/// (`service_common::management_events::PARTITION_LEADER_REGION`).
const SIGNING_REGION: &str = ::service_common::management_events::PARTITION_LEADER_REGION;

/// The endpoint every identity-service call is made at, which is the host the emulated IAM
/// service claims its control plane on.
const ENDPOINT: &str = "https://iam.amazonaws.com";

/// The operations of the identity service this seam binds, in the order a refusal lists them.
///
/// One entry per operation [`call`] builds a request for: the surface a handler that
/// establishes WHAT IT RUNS AS and what that identity may do reads — the role itself, the
/// documents embedded in and attached to it, a managed policy and one of its versions, and
/// the roles, policies and role tags of the account — and the role-and-policy surface an
/// automation handler WRITES. A handler naming any other operation of the service is told the
/// call was not made and why, because a request this module cannot bind is one it would
/// otherwise be guessing at.
pub const BOUND_OPERATIONS: &[&str] = &[
    "GetRole",
    "GetUser",
    "GetRolePolicy",
    "GetPolicy",
    "GetPolicyVersion",
    "ListRoles",
    "ListRolePolicies",
    "ListAttachedRolePolicies",
    "ListPolicies",
    "ListRoleTags",
    "CreateRole",
    "DeleteRole",
    "UpdateAssumeRolePolicy",
    "PutRolePolicy",
    "DeleteRolePolicy",
    "AttachRolePolicy",
    "DetachRolePolicy",
    "CreatePolicy",
    "DeletePolicy",
    "TagRole",
    "UntagRole",
];

/// Make ONE identity-service call: `operation`, with `body` as the operation's input document
/// (the keyword arguments the handler passed its client method).
pub async fn call(
    callback: &EmbeddedEmulator,
    identity: Option<&CallerIdentity>,
    operation: &str,
    body: &str,
    chain: Option<&str>,
) -> Result<Answer, Refusal> {
    let input = Input::read(SERVICE, body)?;
    let observed = Observed::of(SERVICE);
    let client = client(callback, identity, observed.clone(), chain);
    match operation {
        "GetRole" => get_role(&client, &input, &observed).await,
        "GetUser" => get_user(&client, &input, &observed).await,
        "GetRolePolicy" => get_role_policy(&client, &input, &observed).await,
        "GetPolicy" => get_policy(&client, &input, &observed).await,
        "GetPolicyVersion" => get_policy_version(&client, &input, &observed).await,
        "ListRoles" => list_roles(&client, &input, &observed).await,
        "ListRolePolicies" => list_role_policies(&client, &input, &observed).await,
        "ListAttachedRolePolicies" => {
            list_attached_role_policies(&client, &input, &observed).await
        }
        "ListPolicies" => list_policies(&client, &input, &observed).await,
        "ListRoleTags" => list_role_tags(&client, &input, &observed).await,
        "CreateRole" => create_role(&client, &input, &observed).await,
        "DeleteRole" => delete_role(&client, &input, &observed).await,
        "UpdateAssumeRolePolicy" => update_assume_role_policy(&client, &input, &observed).await,
        "PutRolePolicy" => put_role_policy(&client, &input, &observed).await,
        "DeleteRolePolicy" => delete_role_policy(&client, &input, &observed).await,
        "AttachRolePolicy" => attach_role_policy(&client, &input, &observed).await,
        "DetachRolePolicy" => detach_role_policy(&client, &input, &observed).await,
        "CreatePolicy" => create_policy(&client, &input, &observed).await,
        "DeletePolicy" => delete_policy(&client, &input, &observed).await,
        "TagRole" => tag_role(&client, &input, &observed).await,
        "UntagRole" => untag_role(&client, &input, &observed).await,
        other => Err(Refusal::NotMade(format!(
            "the AWS SDK this runtime supplies binds no {other} call on {SERVICE}; the \
             operations it binds are {}",
            BOUND_OPERATIONS.join(", ")
        ))),
    }
}

/// An identity-service client over the in-process callback into this same emulator, signing
/// as the identity the SDK client the guest built was given.
///
/// The transport is in-process, so everything a network client needs and this one cannot have
/// — retries, timeouts, identity caching and stalled-stream protection, each of which needs a
/// sleep implementation the callback transport is not given — is switched off.
///
/// Every request it makes carries the tracing header of the chain this invocation belongs to,
/// where the invocation has one ([`::service_common::xray_trace::ChainHop`]): a call the
/// handler makes is one HOP of the chain that reached the handler.
fn client(
    callback: &EmbeddedEmulator,
    identity: Option<&CallerIdentity>,
    observed: Observed,
    chain: Option<&str>,
) -> Client {
    let config = ::aws_sdk_iam::Config::builder()
        .behavior_version(::aws_sdk_iam::config::BehaviorVersion::latest())
        .region(::aws_sdk_iam::config::Region::new(
            SIGNING_REGION.to_string(),
        ))
        .endpoint_url(ENDPOINT)
        .http_client(callback.http_client())
        .interceptor(observed)
        .retry_config(::aws_sdk_iam::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_iam::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_iam::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_iam::config::StalledStreamProtectionConfig::disabled(),
        );
    let config = match identity {
        Some(identity) => config.credentials_provider(::aws_sdk_iam::config::Credentials::new(
            identity.access_key_id.clone(),
            identity.secret_access_key.clone(),
            identity.token().map(str::to_string),
            None,
            PROVIDER_NAME,
        )),
        None => config.credentials_provider(callback.aws_credentials_provider()),
    };
    // A call the handler makes is one HOP of the chain that reached the handler.
    let config = match chain {
        Some(chain) => config.interceptor(::service_common::xray_trace::ChainHop::carrying(chain)),
        None => config,
    };
    Client::from_conf(config.build())
}

/// `GetRole`: the role itself — its path, its id, its ARN and its trust policy — which is
/// what a handler reads to report the identity it runs as.
async fn get_role(client: &Client, input: &Input, observed: &Observed) -> Result<Answer, Refusal> {
    input.only(&["RoleName"])?;
    let read = client
        .get_role()
        .role_name(input.required_text("RoleName")?)
        .send()
        .await;
    let read = match read {
        Ok(read) => read,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_role(&mut document, "Role", read.role());
    observed.answer(document)
}

/// `GetUser`: the user the call is signed as, or the one it names.
async fn get_user(client: &Client, input: &Input, observed: &Observed) -> Result<Answer, Refusal> {
    input.only(&["UserName"])?;
    let read = client
        .get_user()
        .set_user_name(input.text("UserName")?)
        .send()
        .await;
    let read = match read {
        Ok(read) => read,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    if let Some(user) = read.user() {
        let mut held = Map::new();
        insert_text(&mut held, "Path", Some(user.path()));
        insert_text(&mut held, "UserName", Some(user.user_name()));
        insert_text(&mut held, "UserId", Some(user.user_id()));
        insert_text(&mut held, "Arn", Some(user.arn()));
        insert_timestamp(&mut held, "CreateDate", Some(user.create_date()));
        insert_timestamp(&mut held, "PasswordLastUsed", user.password_last_used());
        insert_permissions_boundary(&mut held, user.permissions_boundary());
        insert_tags(&mut held, user.tags());
        document.insert("User".to_string(), Value::Object(held));
    }
    observed.answer(document)
}

/// `GetRolePolicy`: one document EMBEDDED in the role, which is one half of what the role may
/// do.
async fn get_role_policy(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["RoleName", "PolicyName"])?;
    let read = client
        .get_role_policy()
        .role_name(input.required_text("RoleName")?)
        .policy_name(input.required_text("PolicyName")?)
        .send()
        .await;
    let read = match read {
        Ok(read) => read,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(&mut document, "RoleName", Some(read.role_name()));
    insert_text(&mut document, "PolicyName", Some(read.policy_name()));
    insert_text(
        &mut document,
        "PolicyDocument",
        Some(read.policy_document()),
    );
    observed.answer(document)
}

/// `GetPolicy`: a MANAGED policy, which is the other half — the documents attached to a role
/// rather than embedded in it.
async fn get_policy(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["PolicyArn"])?;
    let read = client
        .get_policy()
        .policy_arn(input.required_text("PolicyArn")?)
        .send()
        .await;
    let read = match read {
        Ok(read) => read,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_policy(&mut document, "Policy", read.policy());
    observed.answer(document)
}

/// `GetPolicyVersion`: the DOCUMENT of one version of a managed policy, which is where the
/// statements of an attached policy are read.
async fn get_policy_version(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["PolicyArn", "VersionId"])?;
    let read = client
        .get_policy_version()
        .policy_arn(input.required_text("PolicyArn")?)
        .version_id(input.required_text("VersionId")?)
        .send()
        .await;
    let read = match read {
        Ok(read) => read,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    if let Some(version) = read.policy_version() {
        let mut held = Map::new();
        insert_text(&mut held, "Document", version.document());
        insert_text(&mut held, "VersionId", version.version_id());
        insert_flag(
            &mut held,
            "IsDefaultVersion",
            Some(version.is_default_version()),
        );
        insert_timestamp(&mut held, "CreateDate", version.create_date());
        document.insert("PolicyVersion".to_string(), Value::Object(held));
    }
    observed.answer(document)
}

/// `ListRoles`: one page of the account's roles.
async fn list_roles(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["PathPrefix", "Marker", "MaxItems"])?;
    let listed = client
        .list_roles()
        .set_path_prefix(input.text("PathPrefix")?)
        .set_marker(input.text("Marker")?)
        .set_max_items(input.integer("MaxItems")?)
        .send()
        .await;
    let listed = match listed {
        Ok(listed) => listed,
        Err(error) => return observed.refused(&error),
    };
    let roles: Vec<Value> = listed.roles().iter().map(role_of).collect();
    let mut document = Map::new();
    document.insert("Roles".to_string(), Value::Array(roles));
    insert_page(&mut document, listed.is_truncated(), listed.marker());
    observed.answer(document)
}

/// `ListRolePolicies`: one page of the names of the documents EMBEDDED in a role.
async fn list_role_policies(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["RoleName", "Marker", "MaxItems"])?;
    let listed = client
        .list_role_policies()
        .role_name(input.required_text("RoleName")?)
        .set_marker(input.text("Marker")?)
        .set_max_items(input.integer("MaxItems")?)
        .send()
        .await;
    let listed = match listed {
        Ok(listed) => listed,
        Err(error) => return observed.refused(&error),
    };
    let names: Vec<Value> = listed
        .policy_names()
        .iter()
        .map(|name| Value::String(name.clone()))
        .collect();
    let mut document = Map::new();
    document.insert("PolicyNames".to_string(), Value::Array(names));
    insert_page(&mut document, listed.is_truncated(), listed.marker());
    observed.answer(document)
}

/// `ListAttachedRolePolicies`: one page of the MANAGED policies attached to a role.
async fn list_attached_role_policies(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["RoleName", "PathPrefix", "Marker", "MaxItems"])?;
    let listed = client
        .list_attached_role_policies()
        .role_name(input.required_text("RoleName")?)
        .set_path_prefix(input.text("PathPrefix")?)
        .set_marker(input.text("Marker")?)
        .set_max_items(input.integer("MaxItems")?)
        .send()
        .await;
    let listed = match listed {
        Ok(listed) => listed,
        Err(error) => return observed.refused(&error),
    };
    let attached: Vec<Value> = listed
        .attached_policies()
        .iter()
        .map(|policy| {
            let mut held = Map::new();
            insert_text(&mut held, "PolicyName", policy.policy_name());
            insert_text(&mut held, "PolicyArn", policy.policy_arn());
            Value::Object(held)
        })
        .collect();
    let mut document = Map::new();
    document.insert("AttachedPolicies".to_string(), Value::Array(attached));
    insert_page(&mut document, listed.is_truncated(), listed.marker());
    observed.answer(document)
}

/// `ListPolicies`: one page of the account's managed policies.
async fn list_policies(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[
        "Scope",
        "OnlyAttached",
        "PathPrefix",
        "PolicyUsageFilter",
        "Marker",
        "MaxItems",
    ])?;
    let scope = input
        .text("Scope")?
        .map(|held| ::aws_sdk_iam::types::PolicyScopeType::from(held.as_str()));
    let usage = input
        .text("PolicyUsageFilter")?
        .map(|held| ::aws_sdk_iam::types::PolicyUsageType::from(held.as_str()));
    let listed = client
        .list_policies()
        .set_scope(scope)
        .set_only_attached(input.boolean("OnlyAttached")?)
        .set_path_prefix(input.text("PathPrefix")?)
        .set_policy_usage_filter(usage)
        .set_marker(input.text("Marker")?)
        .set_max_items(input.integer("MaxItems")?)
        .send()
        .await;
    let listed = match listed {
        Ok(listed) => listed,
        Err(error) => return observed.refused(&error),
    };
    let policies: Vec<Value> = listed.policies().iter().map(policy_of).collect();
    let mut document = Map::new();
    document.insert("Policies".to_string(), Value::Array(policies));
    insert_page(&mut document, listed.is_truncated(), listed.marker());
    observed.answer(document)
}

/// `ListRoleTags`: one page of the tags on a role, which is what an ABAC statement is decided
/// against.
async fn list_role_tags(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["RoleName", "Marker", "MaxItems"])?;
    let listed = client
        .list_role_tags()
        .role_name(input.required_text("RoleName")?)
        .set_marker(input.text("Marker")?)
        .set_max_items(input.integer("MaxItems")?)
        .send()
        .await;
    let listed = match listed {
        Ok(listed) => listed,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_tags(&mut document, listed.tags());
    insert_page(&mut document, listed.is_truncated(), listed.marker());
    observed.answer(document)
}

/// `CreateRole`: a role another workload assumes, with the trust policy that says who may.
async fn create_role(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[
        "Path",
        "RoleName",
        "AssumeRolePolicyDocument",
        "Description",
        "MaxSessionDuration",
        "PermissionsBoundary",
        "Tags",
    ])?;
    let created = client
        .create_role()
        .set_path(input.text("Path")?)
        .role_name(input.required_text("RoleName")?)
        .assume_role_policy_document(input.required_text("AssumeRolePolicyDocument")?)
        .set_description(input.text("Description")?)
        .set_max_session_duration(input.integer("MaxSessionDuration")?)
        .set_permissions_boundary(input.text("PermissionsBoundary")?)
        .set_tags(tags(input)?)
        .send()
        .await;
    let created = match created {
        Ok(created) => created,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_role(&mut document, "Role", created.role());
    observed.answer(document)
}

/// `DeleteRole`: the role given up, which stops every session of it being vended.
async fn delete_role(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["RoleName"])?;
    let sent = client
        .delete_role()
        .role_name(input.required_text("RoleName")?)
        .send()
        .await;
    match sent {
        Ok(_deleted) => observed.answer(Map::new()),
        Err(error) => observed.refused(&error),
    }
}

/// `UpdateAssumeRolePolicy`: the trust policy of a role replaced, which is what changes who
/// may assume it.
async fn update_assume_role_policy(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["RoleName", "PolicyDocument"])?;
    let sent = client
        .update_assume_role_policy()
        .role_name(input.required_text("RoleName")?)
        .policy_document(input.required_text("PolicyDocument")?)
        .send()
        .await;
    match sent {
        Ok(_updated) => observed.answer(Map::new()),
        Err(error) => observed.refused(&error),
    }
}

/// `PutRolePolicy`: one document embedded in a role, put there.
async fn put_role_policy(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["RoleName", "PolicyName", "PolicyDocument"])?;
    let sent = client
        .put_role_policy()
        .role_name(input.required_text("RoleName")?)
        .policy_name(input.required_text("PolicyName")?)
        .policy_document(input.required_text("PolicyDocument")?)
        .send()
        .await;
    match sent {
        Ok(_put) => observed.answer(Map::new()),
        Err(error) => observed.refused(&error),
    }
}

/// `DeleteRolePolicy`: that same document removed.
async fn delete_role_policy(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["RoleName", "PolicyName"])?;
    let sent = client
        .delete_role_policy()
        .role_name(input.required_text("RoleName")?)
        .policy_name(input.required_text("PolicyName")?)
        .send()
        .await;
    match sent {
        Ok(_deleted) => observed.answer(Map::new()),
        Err(error) => observed.refused(&error),
    }
}

/// `AttachRolePolicy`: a managed policy attached to a role, which puts its statements in
/// force on every session of that role.
async fn attach_role_policy(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["RoleName", "PolicyArn"])?;
    let sent = client
        .attach_role_policy()
        .role_name(input.required_text("RoleName")?)
        .policy_arn(input.required_text("PolicyArn")?)
        .send()
        .await;
    match sent {
        Ok(_attached) => observed.answer(Map::new()),
        Err(error) => observed.refused(&error),
    }
}

/// `DetachRolePolicy`: that attachment removed.
async fn detach_role_policy(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["RoleName", "PolicyArn"])?;
    let sent = client
        .detach_role_policy()
        .role_name(input.required_text("RoleName")?)
        .policy_arn(input.required_text("PolicyArn")?)
        .send()
        .await;
    match sent {
        Ok(_detached) => observed.answer(Map::new()),
        Err(error) => observed.refused(&error),
    }
}

/// `CreatePolicy`: a managed policy of the account, which a role is then attached to.
async fn create_policy(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["PolicyName", "Path", "PolicyDocument", "Description", "Tags"])?;
    let created = client
        .create_policy()
        .policy_name(input.required_text("PolicyName")?)
        .set_path(input.text("Path")?)
        .policy_document(input.required_text("PolicyDocument")?)
        .set_description(input.text("Description")?)
        .set_tags(tags(input)?)
        .send()
        .await;
    let created = match created {
        Ok(created) => created,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_policy(&mut document, "Policy", created.policy());
    observed.answer(document)
}

/// `DeletePolicy`: that managed policy given up.
async fn delete_policy(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["PolicyArn"])?;
    let sent = client
        .delete_policy()
        .policy_arn(input.required_text("PolicyArn")?)
        .send()
        .await;
    match sent {
        Ok(_deleted) => observed.answer(Map::new()),
        Err(error) => observed.refused(&error),
    }
}

/// `TagRole`: tags written on a role, which an ABAC statement is decided against.
async fn tag_role(client: &Client, input: &Input, observed: &Observed) -> Result<Answer, Refusal> {
    input.only(&["RoleName", "Tags"])?;
    let sent = client
        .tag_role()
        .role_name(input.required_text("RoleName")?)
        .set_tags(required_tags(input)?)
        .send()
        .await;
    match sent {
        Ok(_tagged) => observed.answer(Map::new()),
        Err(error) => observed.refused(&error),
    }
}

/// `UntagRole`: those tags removed, by key.
async fn untag_role(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["RoleName", "TagKeys"])?;
    let sent = client
        .untag_role()
        .role_name(input.required_text("RoleName")?)
        .set_tag_keys(Some(input.required_text_list("TagKeys")?))
        .send()
        .await;
    match sent {
        Ok(_untagged) => observed.answer(Map::new()),
        Err(error) => observed.refused(&error),
    }
}

/// The `Tags` of a call, read as the key/value pairs the service takes; `None` where the call
/// carries none, so the member is left off the request rather than sent emptied.
fn tags(input: &Input) -> Result<Option<Vec<::aws_sdk_iam::types::Tag>>, Refusal> {
    let held = read_tags(&input.structures("Tags")?)?;
    Ok((!held.is_empty()).then_some(held))
}

/// The same, for an operation whose `Tags` member is REQUIRED: a call carrying none of it
/// stops rather than being sent with the required member emptied.
fn required_tags(input: &Input) -> Result<Option<Vec<::aws_sdk_iam::types::Tag>>, Refusal> {
    Ok(Some(read_tags(&input.required_structures("Tags")?)?))
}

/// One list of tag documents, read as the tags the service takes.
fn read_tags(entries: &[Input]) -> Result<Vec<::aws_sdk_iam::types::Tag>, Refusal> {
    let mut held = Vec::with_capacity(entries.len());
    for tag in entries {
        tag.only(&["Key", "Value"])?;
        held.push(
            ::aws_sdk_iam::types::Tag::builder()
                .key(tag.required_text("Key")?)
                .value(tag.required_text("Value")?)
                .build()
                .map_err(|error| {
                    Refusal::NotMade(format!(
                        "a tag of the call is not one {SERVICE} accepts: {error}"
                    ))
                })?,
        );
    }
    Ok(held)
}

/// Put a ROLE an answer reports on its document, under the member the operation names it by.
fn insert_role(
    document: &mut Map<String, Value>,
    name: &str,
    role: Option<&::aws_sdk_iam::types::Role>,
) {
    if let Some(role) = role {
        document.insert(name.to_string(), role_of(role));
    }
}

/// One role, as the members its own model names them by.
fn role_of(role: &::aws_sdk_iam::types::Role) -> Value {
    let mut held = Map::new();
    insert_text(&mut held, "Path", Some(role.path()));
    insert_text(&mut held, "RoleName", Some(role.role_name()));
    insert_text(&mut held, "RoleId", Some(role.role_id()));
    insert_text(&mut held, "Arn", Some(role.arn()));
    insert_timestamp(&mut held, "CreateDate", Some(role.create_date()));
    insert_text(
        &mut held,
        "AssumeRolePolicyDocument",
        role.assume_role_policy_document(),
    );
    insert_text(&mut held, "Description", role.description());
    insert_number(
        &mut held,
        "MaxSessionDuration",
        role.max_session_duration().map(i64::from),
    );
    insert_permissions_boundary(&mut held, role.permissions_boundary());
    insert_tags(&mut held, role.tags());
    if let Some(used) = role.role_last_used() {
        let mut last = Map::new();
        insert_timestamp(&mut last, "LastUsedDate", used.last_used_date());
        insert_text(&mut last, "Region", used.region());
        held.insert("RoleLastUsed".to_string(), Value::Object(last));
    }
    Value::Object(held)
}

/// Put a MANAGED POLICY an answer reports on its document, under the member the operation
/// names it by.
fn insert_policy(
    document: &mut Map<String, Value>,
    name: &str,
    policy: Option<&::aws_sdk_iam::types::Policy>,
) {
    if let Some(policy) = policy {
        document.insert(name.to_string(), policy_of(policy));
    }
}

/// One managed policy, as the members its own model names them by.
fn policy_of(policy: &::aws_sdk_iam::types::Policy) -> Value {
    let mut held = Map::new();
    insert_text(&mut held, "PolicyName", policy.policy_name());
    insert_text(&mut held, "PolicyId", policy.policy_id());
    insert_text(&mut held, "Arn", policy.arn());
    insert_text(&mut held, "Path", policy.path());
    insert_text(&mut held, "DefaultVersionId", policy.default_version_id());
    insert_number(
        &mut held,
        "AttachmentCount",
        policy.attachment_count().map(i64::from),
    );
    insert_number(
        &mut held,
        "PermissionsBoundaryUsageCount",
        policy.permissions_boundary_usage_count().map(i64::from),
    );
    insert_flag(&mut held, "IsAttachable", Some(policy.is_attachable()));
    insert_text(&mut held, "Description", policy.description());
    insert_timestamp(&mut held, "CreateDate", policy.create_date());
    insert_timestamp(&mut held, "UpdateDate", policy.update_date());
    insert_tags(&mut held, policy.tags());
    Value::Object(held)
}

/// Put the PERMISSIONS BOUNDARY an entity reports on its document: the ceiling on what the
/// documents in force on it can grant.
fn insert_permissions_boundary(
    document: &mut Map<String, Value>,
    boundary: Option<&::aws_sdk_iam::types::AttachedPermissionsBoundary>,
) {
    if let Some(boundary) = boundary {
        let mut held = Map::new();
        insert_text(
            &mut held,
            "PermissionsBoundaryType",
            boundary.permissions_boundary_type().map(|kind| kind.as_str()),
        );
        insert_text(
            &mut held,
            "PermissionsBoundaryArn",
            boundary.permissions_boundary_arn(),
        );
        document.insert("PermissionsBoundary".to_string(), Value::Object(held));
    }
}

/// Put the TAGS an answer reports on its document, each as the key/value pair the service
/// reported.
fn insert_tags(document: &mut Map<String, Value>, tags: &[::aws_sdk_iam::types::Tag]) {
    let held: Vec<Value> = tags
        .iter()
        .map(|tag| {
            let mut entry = Map::new();
            insert_text(&mut entry, "Key", Some(tag.key()));
            insert_text(&mut entry, "Value", Some(tag.value()));
            Value::Object(entry)
        })
        .collect();
    document.insert("Tags".to_string(), Value::Array(held));
}

/// Put the PAGING an answer reports on its document: whether more entries follow and the
/// marker the next page is asked for with.
fn insert_page(document: &mut Map<String, Value>, truncated: bool, marker: Option<&str>) {
    insert_flag(document, "IsTruncated", Some(truncated));
    insert_text(document, "Marker", marker);
}
