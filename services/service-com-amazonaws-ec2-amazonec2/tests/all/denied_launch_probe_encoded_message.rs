// SPDX-License-Identifier: CC0-1.0

//! The ENCODED AUTHORIZATION FAILURE MESSAGE an EC2 refusal hands the caller, and what
//! `sts:DecodeAuthorizationMessage` decodes it back to — driven end to end over the wire EC2
//! speaks (`ec2Query`), with the role, its policy and the scoped session held by the IAM and
//! STS services mounted on the SAME engine, so the decision under test is the one EC2's gate
//! took about the credential `sts:AssumeRole` minted.
//!
//! THE RULE UNDER TEST, in three parts:
//!
//! * `DryRun` is an AUTHORIZATION probe — "checks whether you have the required permissions
//!   for the operation, without actually making the request… If you have the required
//!   permissions, the error response is `DryRunOperation`. Otherwise, it is
//!   `UnauthorizedOperation`" (`RunInstancesRequest$DryRun`) — so the session policy's
//!   explicit `Deny` decides it, and the probe is refused HTTP 403 `UnauthorizedOperation`
//!   naming the principal, the action, the resource the decision was taken on and the rule
//!   that withheld the grant.
//! * A launch is judged on the resource it would CREATE: the `instance` resource type the IAM
//!   Service Authorization Reference lists for `RunInstances`
//!   (`arn:${Partition}:ec2:${Region}:${Account}:instance/${InstanceId}`), which the request
//!   carries no id for — there is no instance until the launch succeeds — so the decision is
//!   taken on that type with every id, and that is the resource the refusal names, whatever
//!   other resources the request references.
//! * A refusal of an operation whose recorded contract hands the caller one closes with an
//!   encoded authorization failure message — "only certain Amazon Web Services operations
//!   return an encoded message… the documentation for an individual operation indicates
//!   whether that operation returns an encoded message"
//!   (`sts:DecodeAuthorizationMessage`) — whichever document in force reached the decision,
//!   and the refusal of an operation recorded without one carries none. That blob decodes —
//!   through STS, which issued it — to the decision EC2 took: not allowed, explicitly denied
//!   exactly when a `Deny` said so, the principal's ARN and unique id, the BARE operation
//!   name as the requested action, the resource it was judged on, the condition keys the
//!   request carried, and the statements it rested on with the actions, resources and
//!   principal each applied to — of which a decision reached by the absence of a grant has
//!   none. A blob this account did not issue is refused
//!   `InvalidAuthorizationMessageException` (HTTP 400) reporting the bytes it named.
//!
//! The rows span what the rule generalises over: the instance type the launch asks for (one,
//! another, none at all), whether the request references a subnet of its own, how the denying
//! statement names the action (the action itself, a wildcard that covers it), whether that
//! statement states a `Sid`, how many instances the launch asks for, and which document
//! reached the decision (a session policy's explicit `Deny`, the absence of any allow). No
//! row is the specification: each is one input along the rule.
//!
//! The four pieces of that drive which are about no launch in particular — the role with its
//! own document, the session minted of it, the split of a refusal into its sentence and its
//! blob, and the decode of that blob — are `pub(crate)` so that the suite covering the SAME
//! rule on the other operation whose refusals carry the trailer
//! ([`super::denied_volume_probe_encoded_message`]) drives it through this one copy rather
//! than a second.

use serde_json::Value;

use super::account_wide_read_authorization::{
    account, call_as, ec2_as, element, emulator, iam_call, statement, ACCOUNT_KEY_ID, REGION,
    STS_API_VERSION,
};
use super::ec2_query::{element as opt_element, elements, error_code, error_message, unescaped};

/// EC2's error code for a caller that may not perform the action it asked for.
const UNAUTHORIZED_OPERATION: &str = "UnauthorizedOperation";

/// EC2's error code for a `DryRun` whose caller DOES hold the action, and the sentence it
/// carries.
const DRY_RUN_OPERATION: &str = "DryRunOperation";
const DRY_RUN_MESSAGE: &str = "Request would have succeeded, but DryRun flag is set.";

/// What a refusal that hands the caller an encoded authorization failure message closes with,
/// before the blob itself — written out here as AWS words it.
const ENCODED_MESSAGE_MARKER: &str = " Encoded authorization failure message: ";

/// The reason clause a refusal reached by a session policy's explicit `Deny` closes with,
/// read from the shared wording every service that reaches that decision states, so this
/// suite asserts the clause rather than a second spelling of it.
fn session_explicit_deny() -> String {
    service_common::iam_authorization::SESSION_POLICY_EXPLICIT_DENY.to_string()
}

/// The reason clause a refusal reached by the absence of any allow in the principal's
/// identity-based documents closes with, read from that same shared wording.
fn no_identity_policy_allows(action: &str) -> String {
    service_common::iam_authorization::no_identity_policy_allows(action)
}

/// The role every row's session is of: one role allowed every action of this service, so the
/// SESSION POLICY is the only thing that decides a row.
const ROLE_NAME: &str = "launch-probe-role";

/// A session policy allowing the two actions a row drives and denying the launch, with
/// `denied` as the `Action` of the `Deny` statement and `sid` as its `Sid` when it states one.
fn allow_then_deny(denied: &str, sid: Option<&str>) -> String {
    let named = sid.map_or(String::new(), |sid| format!("\"Sid\":\"{sid}\","));
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[\
         {{\"Effect\":\"Allow\",\"Action\":[\"ec2:DescribeInstances\",\"ec2:RunInstances\"],\
         \"Resource\":\"*\"}},\
         {{{named}\"Effect\":\"Deny\",\"Action\":\"{denied}\",\"Resource\":\"*\"}}]}}"
    )
}

/// The session a row drives, minted of [`ROLE_NAME`] under the session policy `inline`:
/// answers its access key id, the ARN `sts:GetCallerIdentity` reports for it and the
/// `AssumedRoleId` that identifies the principal.
async fn scoped_session(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    session_name: &str,
    inline: &str,
) -> (String, String, String) {
    assumed(router, case, ROLE_NAME, session_name, Some(inline)).await
}

/// A session of `role_name`, scoped by the session policy `inline` when the mint passes one:
/// answers its access key id, the ARN `sts:GetCallerIdentity` reports for it and its
/// `AssumedRoleId`.
pub(crate) async fn assumed(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    role_name: &str,
    session_name: &str,
    inline: Option<&str>,
) -> (String, String, String) {
    let role_arn = format!("arn:aws:iam::{}:role/{role_name}", account());
    let mut params: Vec<(&str, &str)> = vec![
        ("RoleArn", role_arn.as_str()),
        ("RoleSessionName", session_name),
    ];
    if let Some(inline) = inline {
        params.push(("Policy", inline));
    }
    let (status, assumed) = call_as(
        router,
        "sts",
        STS_API_VERSION,
        ACCOUNT_KEY_ID,
        "AssumeRole",
        &params,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the session of {role_name} must be minted, got {status} {assumed}"
    );
    (
        element(case, &assumed, "AccessKeyId"),
        element(case, &assumed, "Arn"),
        element(case, &assumed, "AssumedRoleId"),
    )
}

/// Create the role `role_name`, assumable by the account's own principals, with `inline` as
/// its one identity-based policy.
pub(crate) async fn role_with(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    role_name: &str,
    inline: &str,
) {
    let trust = format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"AWS\":\"arn:aws:iam::{}:root\"}},\"Action\":\"sts:AssumeRole\"}}]}}",
        account()
    );
    let (status, created) = iam_call(
        router,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the role {role_name} must be created, got {status} {created}"
    );
    let (status, put) = iam_call(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", role_name),
            ("PolicyName", "its-own-grant"),
            ("PolicyDocument", inline),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the policy of {role_name} must be stored, got {status} {put}"
    );
}

/// Create [`ROLE_NAME`], allowed every action of this service, and answer an image id and a
/// subnet id of the account a launch can name.
///
/// Both are read back over the wire from the account's own credential rather than written
/// here: a launch has to name an image the account can resolve and a subnet it holds.
async fn fixtures(router: &smithy_server_runtime::MultiServiceRouter) -> (String, String) {
    let trust = format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"AWS\":\"arn:aws:iam::{}:root\"}},\"Action\":\"sts:AssumeRole\"}}]}}",
        account()
    );
    let (status, created) = iam_call(
        router,
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the role must be created, got {status} {created}"
    );
    let allow_everything = statement("Allow", "*", "*");
    let (status, put) = iam_call(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", "allow-everything"),
            ("PolicyDocument", &allow_everything),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the role's own policy must be stored, got {status} {put}"
    );

    let (status, images) = ec2_as(router, ACCOUNT_KEY_ID, "DescribeImages", &[]).await;
    assert_eq!(
        status, 200,
        "the published images must be readable, got {status} {images}"
    );
    let image_id = elements(&images, "imageId")
        .into_iter()
        .next()
        .expect("a fresh account publishes at least one image");
    let (status, subnets) = ec2_as(router, ACCOUNT_KEY_ID, "DescribeSubnets", &[]).await;
    assert_eq!(
        status, 200,
        "the account's subnets must be readable, got {status} {subnets}"
    );
    let subnet_id = elements(&subnets, "subnetId")
        .into_iter()
        .next()
        .expect("a fresh account holds the default VPC's subnets");
    (image_id, subnet_id)
}

/// The `instance` resource a launch in [`REGION`] is judged on: the type with every id, the
/// request naming no instance that exists yet.
fn judged_instance_resource() -> String {
    ::engine::emulator_arn("ec2", REGION, "instance/*")
}

/// The sentence the refusal states and the blob it hands the caller, split at the marker.
///
/// The blob is one opaque run of characters, which is what lets a client capture it off the
/// message it was given.
pub(crate) fn sentence_and_blob(case: &str, message: &str) -> (String, String) {
    let (sentence, blob) = message.split_once(ENCODED_MESSAGE_MARKER).unwrap_or_else(|| {
        panic!(
            "case {case:?}: a refusal of an operation whose refusals carry one must hand the \
             caller an encoded authorization failure message, got {message:?}"
        )
    });
    assert!(
        !blob.is_empty() && !blob.contains(' '),
        "case {case:?}: expected one opaque encoded message after the marker, got {blob:?}"
    );
    (sentence.to_string(), blob.to_string())
}

/// The document `sts:DecodeAuthorizationMessage` answers for `blob`, decoded through the
/// service that issued it.
pub(crate) async fn decoded(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    blob: &str,
) -> Value {
    let (status, body) = call_as(
        router,
        "sts",
        STS_API_VERSION,
        ACCOUNT_KEY_ID,
        "DecodeAuthorizationMessage",
        &[("EncodedMessage", blob)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the message this account issued must decode, got {status} {body}"
    );
    let rendered = unescaped(&element(case, &body, "DecodedMessage"));
    serde_json::from_str(&rendered).unwrap_or_else(|error| {
        panic!("case {case:?}: the decoded message must be a JSON document ({error}): {rendered}")
    })
}

/// The values of the `{"items":[{"value":…}]}` list at `member` of `held`.
fn values(case: &str, held: &Value, member: &str) -> Vec<String> {
    held[member]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: {member}.items must be a list, got {held}"))
        .iter()
        .map(|entry| entry["value"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The condition keys the decoded message reports, each with its values.
fn conditions(case: &str, document: &Value) -> Vec<(String, Vec<String>)> {
    document["context"]["conditions"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: the conditions must be a list, got {document}"))
        .iter()
        .map(|entry| {
            (
                entry["key"].as_str().unwrap_or_default().to_string(),
                entry["values"]["items"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .map(|value| value["value"].as_str().unwrap_or_default().to_string())
                            .collect()
                    })
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// One row: the launch a session whose scope denies it probes, and how its denying statement
/// is written.
struct Row {
    case: &'static str,
    /// The session name, which the principal a refusal names carries.
    session: &'static str,
    /// The instance type the launch asks for, `None` for a launch naming none.
    instance_type: Option<&'static str>,
    /// Whether the launch references a subnet of its own, which it is NOT judged on.
    names_subnet: bool,
    /// How many instances the launch asks for.
    count: u32,
    /// The `Action` of the `Deny` statement in the session policy.
    denied_action: &'static str,
    /// The `Sid` that statement states, if any.
    sid: Option<&'static str>,
}

/// RULE: a `DryRun` the session policy explicitly denies is refused `UnauthorizedOperation`
/// on the `instance` resource the launch would create, and the refusal hands the caller an
/// encoded authorization failure message that decodes to that very decision.
#[tokio::test]
async fn a_denied_launch_probe_hands_the_caller_the_decision_to_decode() {
    let router = emulator();
    let (image_id, subnet_id) = fixtures(&router).await;
    let rows = vec![
        Row {
            case: "one instance of one type, in a subnet the request names",
            session: "denied-micro",
            instance_type: Some("t3.micro"),
            names_subnet: true,
            count: 1,
            denied_action: "ec2:RunInstances",
            sid: None,
        },
        Row {
            case: "another instance type, no subnet, under a wildcard deny with a Sid",
            session: "denied-large",
            instance_type: Some("m5.large"),
            names_subnet: false,
            count: 3,
            denied_action: "ec2:Run*",
            sid: Some("NoLaunches"),
        },
        Row {
            case: "a launch naming no instance type at all",
            session: "denied-default",
            instance_type: None,
            names_subnet: true,
            count: 2,
            denied_action: "ec2:R*",
            sid: Some("NothingAtAll"),
        },
    ];

    for row in rows {
        let Row {
            case,
            session,
            instance_type,
            names_subnet,
            count,
            denied_action,
            sid,
        } = row;
        let policy = allow_then_deny(denied_action, sid);
        let (akid, principal, assumed_role_id) =
            scoped_session(&router, case, session, &policy).await;

        // The ALLOW half of the same scope, proven live on the same credential: the session
        // is good and the `Deny` is the only variable.
        let (status, read) = ec2_as(&router, &akid, "DescribeInstances", &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the allowed read must be served on the same credential, got \
             {status} {read}"
        );

        let count = count.to_string();
        let mut params: Vec<(&str, &str)> = vec![
            ("ImageId", image_id.as_str()),
            ("MinCount", count.as_str()),
            ("MaxCount", count.as_str()),
            ("DryRun", "true"),
        ];
        if let Some(instance_type) = instance_type {
            params.push(("InstanceType", instance_type));
        }
        if names_subnet {
            params.push(("SubnetId", subnet_id.as_str()));
        }
        let (status, refused) = ec2_as(&router, &akid, "RunInstances", &params).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (403, UNAUTHORIZED_OPERATION),
            "case {case:?}: a denied probe must be refused as unauthorized, got {status} \
             {refused}"
        );
        let (sentence, blob) = sentence_and_blob(case, &error_message(&refused));
        let judged = judged_instance_resource();
        assert_eq!(
            sentence,
            format!(
                "You are not authorized to perform this operation. User: {principal} is not \
                 authorized to perform: ec2:RunInstances on resource: {judged} {}.",
                session_explicit_deny()
            ),
            "case {case:?}: expected the refusal to name the instance resource and the \
             session policy, got {refused}"
        );

        // The blob decodes, through the service that issued it, to the decision EC2 took.
        let document = decoded(&router, case, &blob).await;
        assert_eq!(
            (
                document["allowed"].as_bool(),
                document["explicitDeny"].as_bool(),
                document["context"]["principal"]["arn"].as_str(),
                document["context"]["principal"]["id"].as_str(),
                document["context"]["action"].as_str(),
                document["context"]["resource"].as_str(),
            ),
            (
                Some(false),
                Some(true),
                Some(principal.as_str()),
                Some(assumed_role_id.as_str()),
                Some("RunInstances"),
                Some(judged.as_str()),
            ),
            "case {case:?}: expected the decision the gate took, got {document}"
        );

        // The one statement the decision rested on: the `Deny` of the session policy, as that
        // document wrote it, applied to this session's principal.
        let matched = document["matchedStatements"]["items"]
            .as_array()
            .unwrap_or_else(|| {
                panic!("case {case:?}: matchedStatements.items must be a list, got {document}")
            })
            .clone();
        assert_eq!(
            matched.len(),
            1,
            "case {case:?}: expected exactly the denying statement, got {document}"
        );
        assert_eq!(
            (
                matched[0]["effect"].as_str(),
                matched[0]["statementId"].as_str(),
                values(case, &matched[0], "actions"),
                values(case, &matched[0], "resources"),
                values(case, &matched[0], "principals"),
            ),
            (
                Some("DENY"),
                Some(sid.unwrap_or_default()),
                vec![denied_action.to_string()],
                vec!["*".to_string()],
                vec![assumed_role_id.clone()],
            ),
            "case {case:?}: expected the denying statement as the document wrote it, got \
             {document}"
        );

        // The condition keys the REQUEST carried: the instance type a launch that names one
        // answers, and nothing for one that names none.
        let carried = conditions(case, &document);
        let expected_instance_type = instance_type.map(|instance_type| {
            (
                "ec2:InstanceType".to_string(),
                vec![instance_type.to_string()],
            )
        });
        assert_eq!(
            carried
                .iter()
                .find(|(key, _)| key == "ec2:InstanceType")
                .cloned(),
            expected_instance_type,
            "case {case:?}: expected the launch's own instance type among the condition keys, \
             got {carried:?}"
        );

        // Nothing was launched: the probe is an authorization question, and a refused one
        // performs no work at all.
        let (status, instances) = ec2_as(&router, ACCOUNT_KEY_ID, "DescribeInstances", &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read back must be served, got {status} {instances}"
        );
        assert_eq!(
            opt_element(&instances, "instanceId"),
            None,
            "case {case:?}: a refused probe must launch nothing, got {instances}"
        );
    }
}

/// RULE (the document that decided is not what decides whether a blob is handed over): a
/// launch probe that NO document in force allows at all is refused on the same instance
/// resource, states the absence of the grant, and hands the caller an encoded authorization
/// failure message too — whose decision reports an implicit deny and no matched statement,
/// there being none to name. The refusal of an operation whose recorded contract carries no
/// such message carries none, however it was decided.
#[tokio::test]
async fn a_launch_no_document_allows_is_diagnosed_as_an_implicit_deny() {
    let router = emulator();
    let (image_id, subnet_id) = fixtures(&router).await;

    // A role of its own, allowed one read and nothing else, assumed with NO session policy:
    // the launch is refused by the absence of any allow in the role's own documents.
    let case = "a role whose own policy allows only a read";
    let bare = "reader-only-role";
    role_with(&router, case, bare, &statement("Allow", "ec2:DescribeInstances", "*")).await;
    let (akid, principal, assumed_role_id) =
        assumed(&router, case, bare, "reader-only", None).await;
    let (status, read) = ec2_as(&router, &akid, "DescribeInstances", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the allowed read must be served on the same credential, got {status} \
         {read}"
    );

    let (status, refused) = ec2_as(
        &router,
        &akid,
        "RunInstances",
        &[
            ("ImageId", image_id.as_str()),
            ("InstanceType", "t3.micro"),
            ("MinCount", "1"),
            ("MaxCount", "1"),
            ("DryRun", "true"),
            ("SubnetId", subnet_id.as_str()),
        ],
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (403, UNAUTHORIZED_OPERATION),
        "case {case:?}: the probe must be refused as unauthorized, got {status} {refused}"
    );
    let (sentence, blob) = sentence_and_blob(case, &error_message(&refused));
    let judged = judged_instance_resource();
    assert_eq!(
        sentence,
        format!(
            "You are not authorized to perform this operation. User: {principal} is not \
             authorized to perform: ec2:RunInstances on resource: {judged} {}.",
            no_identity_policy_allows("ec2:RunInstances")
        ),
        "case {case:?}: expected the refusal to state the absence of the grant, got {refused}"
    );

    let document = decoded(&router, case, &blob).await;
    assert_eq!(
        (
            document["allowed"].as_bool(),
            document["explicitDeny"].as_bool(),
            document["context"]["principal"]["arn"].as_str(),
            document["context"]["principal"]["id"].as_str(),
            document["context"]["action"].as_str(),
            document["context"]["resource"].as_str(),
        ),
        (
            Some(false),
            Some(false),
            Some(principal.as_str()),
            Some(assumed_role_id.as_str()),
            Some("RunInstances"),
            Some(judged.as_str()),
        ),
        "case {case:?}: expected an implicit deny of the launch, got {document}"
    );
    assert_eq!(
        document["matchedStatements"]["items"].as_array().map(Vec::len),
        Some(0),
        "case {case:?}: a decision reached by the absence of a grant matches no statement, \
         got {document}"
    );

    // An operation recorded WITHOUT the trailer carries none, decided either way: the same
    // session policy's explicit deny of an interface create, and the absence of any allow of
    // it for the role above.
    let denying = allow_then_deny("ec2:CreateNetworkInterface", Some("NoInterfaces"));
    let (denied_akid, _, _) = scoped_session(&router, case, "interface-denied", &denying).await;
    for (case, akid) in [
        ("an interface create a session policy denies", &denied_akid),
        ("an interface create no document allows", &akid),
    ] {
        let (status, refused) = ec2_as(
            &router,
            akid,
            "CreateNetworkInterface",
            &[("SubnetId", subnet_id.as_str()), ("DryRun", "true")],
        )
        .await;
        let message = error_message(&refused);
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (403, UNAUTHORIZED_OPERATION),
            "case {case:?}: the probe must be refused as unauthorized, got {status} {refused}"
        );
        assert!(
            !message.contains(ENCODED_MESSAGE_MARKER),
            "case {case:?}: an operation recorded without the trailer must carry none, got \
             {message:?}"
        );
    }
}

/// RULE (the control): the same probe under a scope that does NOT deny the launch is
/// authorized, so it answers `DryRunOperation` — which is what separates authorization from
/// request validation — and carries no encoded authorization failure message, there being no
/// decision to diagnose. Nothing is launched either way.
#[tokio::test]
async fn an_allowed_launch_probe_answers_the_dry_run_verdict_and_no_blob() {
    let router = emulator();
    let (image_id, subnet_id) = fixtures(&router).await;
    let allowing = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
                    \"Action\":[\"ec2:DescribeInstances\",\"ec2:RunInstances\"],\
                    \"Resource\":\"*\"}]}";
    let case = "a scope that allows the launch";
    let (akid, _, _) = scoped_session(&router, case, "allowed-probe", allowing).await;
    let (status, answered) = ec2_as(
        &router,
        &akid,
        "RunInstances",
        &[
            ("ImageId", image_id.as_str()),
            ("InstanceType", "t3.micro"),
            ("MinCount", "1"),
            ("MaxCount", "1"),
            ("DryRun", "true"),
            ("SubnetId", subnet_id.as_str()),
        ],
    )
    .await;
    let message = error_message(&answered);
    assert_eq!(
        (status, error_code(&answered).as_str(), message.as_str()),
        (412, DRY_RUN_OPERATION, DRY_RUN_MESSAGE),
        "case {case:?}: expected the dry-run verdict, got {status} {answered}"
    );
    assert!(
        !message.contains(ENCODED_MESSAGE_MARKER),
        "case {case:?}: an authorized probe diagnoses nothing, got {message:?}"
    );
    let (status, instances) = ec2_as(&router, ACCOUNT_KEY_ID, "DescribeInstances", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read back must be served, got {status} {instances}"
    );
    assert_eq!(
        opt_element(&instances, "instanceId"),
        None,
        "case {case:?}: a dry run must launch nothing, got {instances}"
    );
}

/// RULE: a blob this account did not issue decodes to nothing — it is refused
/// `InvalidAuthorizationMessageException` (HTTP 400) reporting the bytes the message named as
/// signed 8-bit values, which is all the service knows of a blob it did not mint.
///
/// The rows differ in what the blob carries (text that is base64 all the same, a message this
/// account DID issue with one byte of its token changed) — so a near miss of a real message is
/// refused exactly as a stranger's blob is.
#[tokio::test]
async fn a_blob_this_account_did_not_issue_is_refused() {
    let router = emulator();
    let (image_id, _) = fixtures(&router).await;
    let policy = allow_then_deny("ec2:RunInstances", None);
    let case = "a message this account issued";
    let (akid, _, _) = scoped_session(&router, case, "tampered-probe", &policy).await;
    let (status, refused) = ec2_as(
        &router,
        &akid,
        "RunInstances",
        &[
            ("ImageId", image_id.as_str()),
            ("InstanceType", "t3.micro"),
            ("MinCount", "1"),
            ("MaxCount", "1"),
            ("DryRun", "true"),
        ],
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (403, UNAUTHORIZED_OPERATION),
        "case {case:?}: the denied probe must be refused as unauthorized, got {status} {refused}"
    );
    let (_, issued) = sentence_and_blob(case, &error_message(&refused));
    // The same blob with one character of the TOKEN it names replaced, so the envelope is
    // this account's and the record it names is not. A character before the final group is
    // chosen, so the change is to bytes the blob names rather than to a final group's unused
    // bits.
    let mut tampered: Vec<char> = issued.chars().collect();
    let changed = tampered.len() - 5;
    tampered[changed] = if tampered[changed] == 'A' { 'B' } else { 'A' };
    let tampered: String = tampered.into_iter().collect();

    // (case, the blob sent, the bytes the refusal must report)
    let rows: Vec<(&str, String, Vec<u8>)> = vec![
        (
            "text that is base64 all the same",
            "not-a-real-encoded-message".to_string(),
            base64_bytes("not-a-real-encoded-message"),
        ),
        (
            "this account's envelope naming a token it never issued",
            tampered.clone(),
            base64_bytes(&tampered),
        ),
    ];
    for (case, blob, carried) in rows {
        let (status, body) = call_as(
            &router,
            "sts",
            STS_API_VERSION,
            ACCOUNT_KEY_ID,
            "DecodeAuthorizationMessage",
            &[("EncodedMessage", blob.as_str())],
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidAuthorizationMessageException"),
            "case {case:?}: expected the refusal of a message this account did not issue, got \
             {status} {body}"
        );
        let rendered: Vec<String> = carried
            .iter()
            .map(|byte| (i16::from(*byte) - if *byte > 127 { 256 } else { 0 }).to_string())
            .collect();
        assert_eq!(
            error_message(&body),
            format!("Message is not valid: [{}]", rendered.join(", ")),
            "case {case:?}: the refusal must report the bytes the blob named, got {body}"
        );
    }

    // The message this account DID issue still decodes, so the refusals above are about the
    // blobs sent and not about the registry having been emptied.
    let document = decoded(&router, case, &issued).await;
    assert_eq!(
        document["explicitDeny"].as_bool(),
        Some(true),
        "case {case:?}: the issued message must still decode, got {document}"
    );
}

/// The bytes a URL-safe, unpadded base64 blob names, read with the `base64` crate — the one
/// base64 implementation this crate already depends on.
///
/// An encoded authorization message travels in the URL-safe alphabet with its padding
/// stripped (RFC 4648 §5), and the bits left over in a final group that is not a whole number
/// of bytes carry no data, so they are discarded rather than required to be zero: the engine
/// is configured for exactly that, which is the reading a DECODER of someone else's blob is
/// held to.
fn base64_bytes(blob: &str) -> Vec<u8> {
    use ::base64::Engine as _;

    let engine = ::base64::engine::GeneralPurpose::new(
        &::base64::alphabet::URL_SAFE,
        ::base64::engine::GeneralPurposeConfig::new()
            .with_encode_padding(false)
            .with_decode_padding_mode(::base64::engine::DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    engine
        .decode(blob)
        .unwrap_or_else(|error| panic!("{blob:?} must be base64 ({error})"))
}
