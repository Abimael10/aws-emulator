// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arm of AWS Amplify's own control plane: the BRANCH an
//! `AWS::Amplify::Branch` declares. One file of [`super`]'s per-type arms, dispatched to by its
//! `Provisioner`, beside the arm of the app every branch hangs off ([`super::amplify_apps`]).
//!
//! ## Why the branch is really created
//!
//! A branch is the unit AWS Amplify hosts: `GetBranch`, `ListBranches`, `UpdateBranch`,
//! `CreateDeployment` and `StartDeployment` each address one by the `branchName` it was created
//! under, inside the app its `appId` names. A template publishes the branch's ARN through `Ref`
//! and the first thing a client does with the deployed stack is call that service about the
//! branch. A stack that reported the branch created while nothing created it publishes a handle
//! that resolves to nothing, so every one of those calls answers `NotFoundException` — the
//! stack completed and the client is broken.
//!
//! ## The identity is that service's
//!
//! `CreateBranch` takes the app as its URI label and the branch's NAME in its payload, and
//! answers the branch carrying the `branchArn` it built — which is the app's own ARN, around the
//! `appId` that service minted, with the branch's name appended. "Ref returns the ARN of the
//! branch", which is the type's primary identifier, so that ARN is this resource's physical id
//! ([`super::super::template::identity_is_service_assigned`] names the type). The NAME is the
//! declaration's own `BranchName`, and no call of that service renames a branch — every one of
//! them addresses a branch BY its name — so a re-declared name asks for a different branch,
//! which CloudFormation replaces.
//!
//! ## What the declaration is
//!
//! Most of this type's properties are the member of the same name with a lower-cased initial
//! ([`camel_cased`]). Three are not, and each is converted here exactly as the app's arm
//! converts the same spellings:
//!
//! * `AppId` is the create's URI label and the update's, so it is lifted out of the payload;
//! * `EnvironmentVariables` is a LIST of `{Name, Value}` pairs while the member is a MAP;
//! * `BasicAuthConfig` states the user and the password apart, while the member pair is
//!   `enableBasicAuth` beside the `basicAuthCredentials` that service takes "base64-encode[d]
//!   … in the format `user:password`".
//!
//! `Tags` is a LIST of `{Key, Value}` pairs for this type, converted to the `TagMap` that
//! call's own `tags` member takes, and carrying the tags CloudFormation gives the resource
//! beside the ones the declaration asks for.
//!
//! A property outside [`AMPLIFY_BRANCH_MAPPED_PROPERTIES`] is REFUSED rather than dropped: a
//! resource reported as created with less than its declaration asked for is not the resource
//! the template described.
//!
//! ## A refusal about the app the declaration REFERENCES, and about the branch itself
//!
//! The create names TWO resources: the branch it makes, and the app it asks to make that branch
//! under — which the declaration only references, by the `AppId` it states. Either way the
//! refusal says the branch this handler was asked for is not there, and this provider reports
//! it in the resource-provider plugin's own not-found sentence
//! ([`amplify_not_found_reported`]) rather than forwarding AWS Amplify's own sentence, which
//! names the app in that service's terms and no CloudFormation type at all. The identifier the
//! sentence quotes is the one the handler holds for the branch — the ARN that service assigns,
//! which a create has not been answered yet.

use super::*;

/// The operations this module calls, as AWS Amplify's own model names them.
const CREATE_BRANCH: &str = "CreateBranch";
const GET_BRANCH: &str = "GetBranch";
const UPDATE_BRANCH: &str = "UpdateBranch";
const DELETE_BRANCH: &str = "DeleteBranch";

/// The member every one of those operations answers the branch record in.
const BRANCH_MEMBER: &str = "branch";

/// The member the branch's name reaches `CreateBranch`'s payload as, and the property the
/// declaration states it in.
const BRANCH_NAME_MEMBER: &str = "branchName";
const BRANCH_NAME_PROPERTY: &str = "BranchName";

/// The property the declaration states the app the branch belongs to in, which every call on a
/// branch carries as its first URI label.
const APP_ID_PROPERTY: &str = "AppId";

/// The property the declared tag list lives in, and the member it reaches `CreateBranch` as.
const BRANCH_TAGS_PROPERTY: &str = "Tags";
const BRANCH_TAGS_MEMBER: &str = "tags";

/// The properties this arm maps onto `CreateBranch`'s members, and so the ones a declaration
/// may carry: every property of the type whose value this arm really applies.
///
/// Source: the type's own entry in the "AWS resource and property types reference" read beside
/// `CreateBranch`'s own members (`smithy_query(query="operation", operation="CreateBranch")`).
const AMPLIFY_BRANCH_MAPPED_PROPERTIES: &[&str] = &[
    "AppId",
    "Backend",
    "BasicAuthConfig",
    "BranchName",
    "BuildSpec",
    "ComputeRoleArn",
    "Description",
    "EnableAutoBuild",
    "EnablePerformanceMode",
    "EnablePullRequestPreview",
    "EnableSkewProtection",
    "EnvironmentVariables",
    "Framework",
    "PullRequestEnvironmentName",
    "Stage",
    "Tags",
];

/// The attributes an `AWS::Amplify::Branch` publishes, as `(attribute, member)`: the ARN AWS
/// Amplify built for the branch and the name it records it under — both read off that service's
/// own report of the branch rather than recomputed here.
const AMPLIFY_BRANCH_ATTRIBUTE_MEMBERS: &[(&str, &str)] =
    &[("Arn", "branchArn"), ("BranchName", "branchName")];

/// The attribute carrying the ARN that is this type's primary identifier.
const BRANCH_ARN_ATTRIBUTE: &str = "Arn";

/// The properties the branch's own read reports back under the same names, for drift detection,
/// as `(property, member)`: the SCALAR members `GetBranch` answers that a declaration also
/// states.
///
/// The configuration documents and the maps are left out: what the read reports are that
/// service's own member names and shapes, and a property the read does not report is not
/// evidence of anything ([`super::drift`]).
const AMPLIFY_BRANCH_REPORTED_PROPERTIES: &[(&str, &str)] = &[
    ("BranchName", "branchName"),
    ("Description", "description"),
    ("Framework", "framework"),
    ("Stage", "stage"),
    ("EnableAutoBuild", "enableAutoBuild"),
    ("EnablePerformanceMode", "enablePerformanceMode"),
    ("EnablePullRequestPreview", "enablePullRequestPreview"),
    ("EnableSkewProtection", "enableSkewProtection"),
    ("PullRequestEnvironmentName", "pullRequestEnvironmentName"),
    ("BuildSpec", "buildSpec"),
    ("ComputeRoleArn", "computeRoleArn"),
];

/// Create the branch an `AWS::Amplify::Branch` declares, in AWS Amplify — so a client that
/// deployed the stack reads it back through `GetBranch` for the ARN the stack publishes, finds
/// it in `ListBranches` for the app, and can run a deployment against it.
///
/// The declaration IS the request once its members are converted
/// ([`declared_branch_members`]), under the app its `AppId` names, with the declared tags beside
/// the ones CloudFormation gives the resource ([`tags_of`]).
///
/// The identity is that service's: the `branchArn` it answered becomes this resource's physical
/// id — the type's primary identifier — and the two values the type publishes are read off the
/// branch it answered.
pub(super) async fn create_amplify_branch(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, AMPLIFY_BRANCH_MAPPED_PROPERTIES)?;
    // The app is read off the DECLARATION here, not off the identity: the declaration is the
    // resolved request (a reference to the app resource carries the id that service minted by
    // now), while the identity the naming rules computed is only a stand-in until this create
    // answers.
    let app_id = declared_text(&resource.properties, APP_ID_PROPERTY).unwrap_or_default();
    let mut request = declared_branch_members(&resource.properties)?;
    request.insert(
        BRANCH_NAME_MEMBER.to_string(),
        Value::String(branch_name(&resource.properties, resource)),
    );
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert(
            BRANCH_TAGS_MEMBER.to_string(),
            Value::Object(tags_as_map(&tags)),
        );
    }
    let answered = call_rest_json_named(
        ctx,
        AMPLIFY_SIGNING_NAME,
        region,
        CREATE_BRANCH,
        "POST",
        &branches_uri(&app_id),
        Some(&Value::Object(request)),
    )
    .await
    // A create this service refuses as not-found is the branch it was asked to make not being
    // there, which this provider reports in the plugin's own sentence around the type it manages
    // and the identifier it holds for the branch ([`amplify_not_found_reported`]) — and a create
    // holds none: the branch is identified by the ARN that service assigns when it answers.
    .map_err(|refusal| {
        amplify_not_found_reported(refusal, AMPLIFY_BRANCH, AMPLIFY_NO_IDENTIFIER)
    })?;
    let attributes = branch_attributes(&answered)?;
    Ok(Created {
        // "Ref returns the ARN of the branch", which AWS Amplify built around the `appId` it
        // minted — not a value a naming rule could have computed.
        physical_resource_id: attributes.get(BRANCH_ARN_ATTRIBUTE).cloned(),
        attributes,
    })
}

/// Apply a changed declaration to the branch the stack created, through `UpdateBranch` — the one
/// call that changes a branch that exists.
///
/// It takes the app and the branch's name as its URI labels together with the declared
/// configuration, so a member the declaration no longer states leaves the branch without it
/// rather than keeping the one an earlier deployment gave it. The branch keeps the name and the
/// ARN it was created under, so the handle the stack publishes is unchanged.
///
/// `Ok(None)` is a change that call cannot make, so the deployment reports it as not applied
/// rather than as a success that dropped it: a changed `Tags` list, which `UpdateBranch` does
/// not carry — a branch's tags are that service's own tagging calls.
pub(super) async fn update_amplify_branch(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    if declared.get(BRANCH_TAGS_PROPERTY) != resource.properties.get(BRANCH_TAGS_PROPERTY) {
        return Ok(None);
    }
    // The same refusal the create is held to, over the document THIS deployment declares: a
    // property no arm applies must not be reported as an applied change any more than as a
    // created resource.
    refuse_unmapped_properties(
        &StoredResource {
            properties: declared.clone(),
            ..resource.clone()
        },
        AMPLIFY_BRANCH_MAPPED_PROPERTIES,
    )?;
    let request = declared_branch_members(declared)?;
    let (app_id, name) = branch_identity(resource);
    let answered = call_rest_json_named(
        ctx,
        AMPLIFY_SIGNING_NAME,
        region,
        UPDATE_BRANCH,
        "POST",
        &branch_uri(&app_id, &name),
        Some(&Value::Object(request)),
    )
    .await
    // The same report for the update, whose handler DOES hold an identifier for the branch: the
    // ARN this resource's physical id carries.
    .map_err(|refusal| {
        amplify_not_found_reported(
            refusal,
            AMPLIFY_BRANCH,
            amplify_held_identifier(&resource.physical_resource_id),
        )
    })?;
    Ok(Some(Created::attributes(branch_attributes(&answered)?)))
}

/// Delete the branch a stack created, by the name it was created under, inside the app it
/// belongs to. A branch that service no longer holds (`NotFoundException`, judged on the CODE
/// and never on a message that carries the request's own values) is already in the state the
/// delete asks for.
pub(super) async fn delete_amplify_branch(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let (app_id, name) = branch_identity(resource);
    match call_rest_json_named(
        ctx,
        AMPLIFY_SIGNING_NAME,
        region,
        DELETE_BRANCH,
        "DELETE",
        &branch_uri(&app_id, &name),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == AMPLIFY_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The branch's configuration as AWS Amplify reports it, for drift detection: `GetBranch`'s own
/// record, read by the name the stack published inside the app it belongs to
/// ([`AMPLIFY_BRANCH_REPORTED_PROPERTIES`]). One that service no longer holds was deleted.
pub(super) async fn describe_amplify_branch(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let (app_id, name) = branch_identity(resource);
    match call_rest_json_named(
        ctx,
        AMPLIFY_SIGNING_NAME,
        region,
        GET_BRANCH,
        "GET",
        &branch_uri(&app_id, &name),
        None,
    )
    .await
    {
        Ok(answer) => Ok(answer
            .get(BRANCH_MEMBER)
            .map(|branch| reported_as_properties(branch, AMPLIFY_BRANCH_REPORTED_PROPERTIES))),
        Err(refusal) if refusal.code == AMPLIFY_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The URI the branches of one app are bound to, as that model's `@http` traits declare
/// (`POST`/`GET /apps/{appId}/branches`), with the `appId` `@httpLabel` escaped as a single
/// path segment ([`percent_encoded`]) so a value carrying URI structure addresses one app
/// rather than another path.
fn branches_uri(app_id: &str) -> String {
    format!("/apps/{}/branches", percent_encoded(app_id))
}

/// The URI ONE branch is addressed at: the app's branch collection plus the `branchName`
/// `@httpLabel`, escaped the same way.
fn branch_uri(app_id: &str, branch_name: &str) -> String {
    format!(
        "{}/{}",
        branches_uri(app_id),
        percent_encoded(branch_name)
    )
}

/// The app and the branch name every call on the branch takes.
///
/// Read off the ARN that is this resource's physical id where it carries both
/// (`…:apps/<appId>/branches/<branchName>`, which is the identifier AWS Amplify answered), and
/// otherwise off the declaration's own `AppId` and the name this resource publishes — which is
/// the position a record is in while the create's stand-in identity is still what it carries.
fn branch_identity(resource: &StoredResource) -> (String, String) {
    if let Some(identity) = arn_identity(&resource.physical_resource_id) {
        return identity;
    }
    (
        declared_text(&resource.properties, APP_ID_PROPERTY).unwrap_or_default(),
        branch_name(&resource.properties, resource),
    )
}

/// The app and the branch name one branch ARN carries, or `None` for a value that is not one:
/// the ARN's resource part is `apps/<appId>/branches/<branchName>`, so the two are its last
/// three slash-separated segments around the collection the branch hangs off.
fn arn_identity(physical_resource_id: &str) -> Option<(String, String)> {
    let (apps, branch_name) = physical_resource_id.rsplit_once("/branches/")?;
    let app_id = apps.rsplit_once("apps/")?.1;
    (!app_id.is_empty() && !branch_name.is_empty() && !app_id.contains('/'))
        .then(|| (app_id.to_string(), branch_name.to_string()))
}

/// The NAME the branch is created under: the `BranchName` the declaration states — the type
/// marks it required — and, for a declaration that states none, the name this resource
/// publishes, so the create carries a name that member accepts rather than an empty one.
fn branch_name(declared: &Value, resource: &StoredResource) -> String {
    declared_text(declared, BRANCH_NAME_PROPERTY).unwrap_or_else(|| {
        resource
            .attributes
            .get(BRANCH_NAME_PROPERTY)
            .cloned()
            .unwrap_or_default()
    })
}

/// One declared property document as the request members AWS Amplify takes: every member name
/// re-cased ([`camel_cased`]), the two properties whose shape differs converted, and the app,
/// the branch name and the tags left out — the URI carries the first, the caller adds the second
/// and the create adds the third as the map that call takes — so one reader serves the create
/// and the update.
fn declared_branch_members(declared: &Value) -> Result<Map<String, Value>, Refusal> {
    let Some(declared) = declared.as_object() else {
        // A declaration that is not an object states no member of the request.
        return Ok(Map::new());
    };
    let mut request = Map::new();
    for (property, value) in declared {
        if property == APP_ID_PROPERTY
            || property == BRANCH_NAME_PROPERTY
            || property == BRANCH_TAGS_PROPERTY
        {
            continue;
        }
        match property.as_str() {
            ENVIRONMENT_VARIABLES_PROPERTY => {
                request.insert(
                    ENVIRONMENT_VARIABLES_MEMBER.to_string(),
                    Value::Object(name_value_map(value)),
                );
            }
            BASIC_AUTH_CONFIG_PROPERTY => {
                for (member, carried) in basic_auth_members(value) {
                    request.insert(member, carried);
                }
            }
            _ => {
                request.insert(lower_initial(property), camel_cased(value, 0)?);
            }
        }
    }
    Ok(request)
}

/// The attributes one answered branch reports ([`AMPLIFY_BRANCH_ATTRIBUTE_MEMBERS`]).
///
/// An answer carrying no ARN is reported as a refusal rather than recorded as a created
/// resource: the ARN is this type's primary identifier, so there would be nothing for `Ref` to
/// resolve to and nothing for the stack's own deletion to act on.
fn branch_attributes(answered: &Value) -> Result<BTreeMap<String, String>, Refusal> {
    let Some(branch) = answered.get(BRANCH_MEMBER) else {
        return Err(no_branch_answered());
    };
    let attributes = reported_attributes(branch, AMPLIFY_BRANCH_ATTRIBUTE_MEMBERS);
    if !attributes.contains_key(BRANCH_ARN_ATTRIBUTE) {
        return Err(no_branch_answered());
    }
    Ok(attributes)
}

/// The refusal an answer carrying no identified branch is reported as.
fn no_branch_answered() -> Refusal {
    Refusal {
        code: "InternalFailure".to_string(),
        message: "AWS Amplify answered a created branch without an ARN".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a declared property is the API member of the same name with a lower-cased initial,
    /// except for the ones this type states differently — the environment variables' shape and
    /// the basic-authorization pair — and the three the URI, the caller and the tag map carry.
    #[test]
    fn a_declaration_is_the_requests_members() {
        let declared = ::serde_json::json!({
            "AppId": "d1a2b3c4d5e6f7",
            "BranchName": "release/1.0",
            "Description": "the release branch",
            "Stage": "PRODUCTION",
            "Framework": "React",
            "EnableAutoBuild": false,
            "EnvironmentVariables": [
                {"Name": "STAGE", "Value": "prod"},
                {"Name": "REGION", "Value": "us-east-1"}
            ],
            "BasicAuthConfig": {
                "EnableBasicAuth": true,
                "Username": "studio",
                "Password": "secret"
            },
            "Backend": {"StackArn": "arn:aws:cloudformation:us-east-1:000000000000:stack/b/1"},
            "Tags": [{"Key": "owner", "Value": "studio"}]
        });
        let request = declared_branch_members(&declared).expect("the declaration is mapped");

        assert_eq!(
            request.get("stage").and_then(Value::as_str),
            Some("PRODUCTION"),
            "case \"a re-cased scalar\": expected the member the call takes, got {request:?}"
        );
        assert_eq!(
            request.get("enableAutoBuild"),
            Some(&Value::Bool(false)),
            "case \"a re-cased flag\": expected the member the call takes, got {request:?}"
        );
        assert_eq!(
            request.get(ENVIRONMENT_VARIABLES_MEMBER),
            Some(&::serde_json::json!({"STAGE": "prod", "REGION": "us-east-1"})),
            "case \"the environment variables\": expected the pairs as a map, got {request:?}"
        );
        assert_eq!(
            request.get("enableBasicAuth"),
            Some(&Value::Bool(true)),
            "case \"basic authorization\": expected the flag the call takes, got {request:?}"
        );
        assert_eq!(
            request.get("basicAuthCredentials").and_then(Value::as_str),
            // `studio:secret` base64-encoded, which is the form that member documents.
            Some("c3R1ZGlvOnNlY3JldA=="),
            "case \"basic authorization\": expected base64 user:password, got {request:?}"
        );
        assert_eq!(
            request.get("backend"),
            Some(&::serde_json::json!({
                "stackArn": "arn:aws:cloudformation:us-east-1:000000000000:stack/b/1"
            })),
            "case \"a nested structure\": expected every member re-cased, got {request:?}"
        );
        for member in [
            "appId",
            BRANCH_NAME_MEMBER,
            BRANCH_TAGS_MEMBER,
        ] {
            assert!(
                !request.contains_key(member),
                "case \"what the URI and the caller carry\": expected no {member}, got \
                 {request:?}"
            );
        }
    }

    /// One row of [`a_branch_is_addressed_by_the_arn_that_service_answered`]: the case, the
    /// physical resource id read, and the app and the branch name that id carries, where it
    /// carries them.
    type IdentityRow = (
        &'static str,
        &'static str,
        Option<(&'static str, &'static str)>,
    );

    /// RULE: the app and the branch name every call on a branch takes are the ones the ARN AWS
    /// Amplify answered carries; a physical id that is not such an ARN leaves the declaration's
    /// own values in force.
    #[test]
    fn a_branch_is_addressed_by_the_arn_that_service_answered() {
        let rows: &[IdentityRow] = &[
            (
                "the ARN that service answered",
                "arn:aws:amplify:us-east-1:000000000000:apps/d1a2b3c4d5e6f7/branches/main",
                Some(("d1a2b3c4d5e6f7", "main")),
            ),
            (
                "a branch name carrying a slash",
                "arn:aws:amplify:us-east-1:000000000000:apps/d1a2b3c4d5e6f7/branches/release/1.0",
                Some(("d1a2b3c4d5e6f7", "release/1.0")),
            ),
            (
                "the stand-in the naming rules computed",
                "the Arn of main is not available until it has been created",
                None,
            ),
            ("an app's own ARN", "arn:aws:amplify:us-east-1:000000000000:apps/d1a2b3c4d5e6f7", None),
        ];
        for (case, physical_resource_id, wanted) in rows {
            let read = arn_identity(physical_resource_id);
            let wanted = wanted.map(|(app, name)| (app.to_string(), name.to_string()));
            assert_eq!(
                read, wanted,
                "case {case:?}: expected {wanted:?} from {physical_resource_id:?}, got {read:?}"
            );
        }
    }
}
