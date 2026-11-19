// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of HOW the thing-principal relation and the resource-tagging
//! plane WORD a refusal, and of which refusal each condition is.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! Two rules are under test.
//!
//! 1. A refusal of the thing-principal relation is worded after the CALL that failed, not
//!    after the resource family it could not resolve: the sentence names the action the
//!    caller asked for, the principal the request carried and the thing it addressed, with
//!    no account qualification. And the PRINCIPAL is classified before it is resolved — a
//!    value that is no principal address at all is the modelled `InvalidRequestException`
//!    (400) naming the type it could not read, while a well-formed address this account
//!    holds no principal under is the modelled `ResourceNotFoundException` (404), whatever
//!    resource type the address names.
//! 2. The operations that WRITE the tag set of a resource addressed by `resourceArn` alone
//!    state a missing THING GROUP in that plane's own sentence, which is not the one the
//!    operations addressing the group itself state.
//!
//! The tables span what those rules generalise over rather than one recorded request: the
//! three operations of the relation, thing and group names of several forms and lengths
//! (including the modelled maximum), both forms a principal is named in, values that are no
//! address at all in several shapes, and well-formed addresses of six resource types — one
//! of which names a resource the account really holds and is still no principal of a thing.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::describe_endpoint::percent_encode;

use super::{signed_for_iot, ACCOUNT_ID, REGION};

/// The `x-amzn-errortype` this service names its legacy bad-request shape with: the shape
/// name qualified with the API doc namespace (`service::ShapeNamespacedErrorType`).
const INVALID_REQUEST: &str = "InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/";

/// The `x-amzn-errortype` a modelled not-found carries.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// An Amazon Cognito identity of the `<region>:<id>` form a thing principal may be named
/// in: a principal no record of this account has to exist for, because identity pools live
/// in another service.
const COGNITO_IDENTITY: &str = "us-east-1:44444444-4444-4444-4444-444444444444";

/// An IoT policy document the policy this suite creates is stored with: the document is not
/// what the suite is about, so one valid document serves it.
const POLICY_DOCUMENT: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"iot:*","Resource":"*"}]}"#;

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// The `message` member a modelled error carries.
    fn message(&self) -> Option<String> {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// A string member of the body, or `None` when it is absent or not a string.
    fn text(&self, name: &str) -> Option<String> {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The `principals` a `ListThingPrincipals` answer reported, in its order.
    fn principals(&self) -> Vec<String> {
        self.body
            .get("principals")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The tag keys a `ListTagsForResource` answer reported.
    fn tag_keys(&self) -> Vec<String> {
        self.body
            .get("tags")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|tag| tag.get("Key").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached
/// the way the production router reaches it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// One request, with any number of extra headers — a thing principal is carried in one
    /// of them (`principal` is bound to `x-amzn-principal`).
    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        headers: &[(&str, &str)],
    ) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot(REGION))
            .header("content-type", "application/json");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    /// `CreateThing`: `POST /things/{thingName}`, asserted to have registered the thing.
    async fn given_thing(&self, thing_name: &str) {
        let created = self
            .call("POST", &format!("/things/{thing_name}"), Some(&json!({})), &[])
            .await;
        assert_eq!(
            created.status, 200,
            "setup: registering the thing {thing_name:?} must succeed, got {created:?}"
        );
    }

    /// `CreateThingGroup`: `POST /thing-groups/{thingGroupName}`, asserted to have created
    /// the group.
    async fn given_group(&self, thing_group_name: &str) {
        let created = self
            .call(
                "POST",
                &format!("/thing-groups/{thing_group_name}"),
                Some(&json!({})),
                &[],
            )
            .await;
        assert_eq!(
            created.status, 200,
            "setup: creating the group {thing_group_name:?} must succeed, got {created:?}"
        );
    }

    /// `CreatePolicy`: `POST /policies/{policyName}`, asserted to have stored the policy.
    async fn given_policy(&self, policy_name: &str) {
        let created = self
            .call(
                "POST",
                &format!("/policies/{policy_name}"),
                Some(&json!({ "policyDocument": POLICY_DOCUMENT })),
                &[],
            )
            .await;
        assert_eq!(
            created.status, 200,
            "setup: creating the policy {policy_name:?} must succeed, got {created:?}"
        );
    }

    /// `CreateKeysAndCertificate`: `POST /keys-and-certificate`, answering the ARN of the
    /// certificate it issued — a principal this account really holds.
    async fn given_certificate(&self) -> String {
        let issued = self
            .call("POST", "/keys-and-certificate?setAsActive=true", None, &[])
            .await;
        assert_eq!(
            issued.status, 200,
            "setup: issuing a certificate must succeed, got {issued:?}"
        );
        issued
            .text("certificateArn")
            .expect("an issued certificate carries its ARN")
    }

    /// `AttachThingPrincipal`: `PUT /things/{thingName}/principals`, with the principal in
    /// the header the model binds it to.
    async fn attach(&self, thing_name: &str, principal: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/things/{thing_name}/principals"),
            None,
            &[("x-amzn-principal", principal)],
        )
        .await
    }

    /// `DetachThingPrincipal`: `DELETE /things/{thingName}/principals`.
    async fn detach(&self, thing_name: &str, principal: &str) -> Reply {
        self.call(
            "DELETE",
            &format!("/things/{thing_name}/principals"),
            None,
            &[("x-amzn-principal", principal)],
        )
        .await
    }

    /// `ListThingPrincipals`: `GET /things/{thingName}/principals`.
    async fn list_principals(&self, thing_name: &str) -> Reply {
        self.call("GET", &format!("/things/{thing_name}/principals"), None, &[])
            .await
    }

    /// `DescribeThingGroup`: `GET /thing-groups/{thingGroupName}`.
    async fn describe_group(&self, thing_group_name: &str) -> Reply {
        self.call("GET", &format!("/thing-groups/{thing_group_name}"), None, &[])
            .await
    }

    /// `TagResource`: `POST /tags`.
    async fn tag(&self, resource_arn: &str, key: &str, value: &str) -> Reply {
        self.call(
            "POST",
            "/tags",
            Some(&json!({
                "resourceArn": resource_arn,
                "tags": [{ "Key": key, "Value": value }],
            })),
            &[],
        )
        .await
    }

    /// `UntagResource`: `POST /untag`.
    async fn untag(&self, resource_arn: &str, key: &str) -> Reply {
        self.call(
            "POST",
            "/untag",
            Some(&json!({ "resourceArn": resource_arn, "tagKeys": [key] })),
            &[],
        )
        .await
    }

    /// `ListTagsForResource`: `GET /tags?resourceArn=…`.
    async fn list_tags(&self, resource_arn: &str) -> Reply {
        self.call(
            "GET",
            &format!("/tags?resourceArn={}", percent_encode(resource_arn)),
            None,
            &[],
        )
        .await
    }
}

/// The sentence `AttachThingPrincipal` states a `thingName` the registry does not hold
/// with: the action that failed, the principal the request carried and the thing it
/// addressed, with no account qualification and no trailing period.
fn attach_missing_thing_message(principal: &str, thing_name: &str) -> String {
    format!(
        "Failed to attach the principal {principal} to thing {thing_name} because the thing \
         does not exist in your account"
    )
}

/// The same sentence for `DetachThingPrincipal`, which moves the principal the other way.
fn detach_missing_thing_message(principal: &str, thing_name: &str) -> String {
    format!(
        "Failed to detach the principal {principal} from thing {thing_name} because the \
         thing does not exist in your account"
    )
}

/// The sentence `ListThingPrincipals` states a `thingName` the registry does not hold with.
/// AWS leaves no space between `thing` and the name.
fn list_principals_missing_thing_message(thing_name: &str) -> String {
    format!(
        "Failed to list principals for thing{thing_name} because the thing does not exist in \
         your account"
    )
}

/// The sentence `AttachThingPrincipal` states a well-formed principal address this account
/// holds no principal under with.
fn attach_missing_principal_message(principal: &str, thing_name: &str) -> String {
    format!(
        "Can't attach the principal {principal} to thing {thing_name} because the principal \
         does not exist in your account"
    )
}

/// The same sentence for `DetachThingPrincipal`.
fn detach_missing_principal_message(principal: &str, thing_name: &str) -> String {
    format!(
        "Can't detach the principal {principal} from thing {thing_name} because the \
         principal does not exist in your account"
    )
}

/// The sentence a value that is no principal address at all is refused with: the type that
/// could not be read, and the value exactly as the request sent it.
fn invalid_principal_type_message(principal: &str) -> String {
    format!("Invalid principal type: {principal}")
}

/// The sentence the TAG plane states a missing thing group with: the family noun as two
/// words, the name the ARN carried, and a trailing period — no account qualification.
fn tagged_group_missing_message(thing_group_name: &str) -> String {
    format!("Thing Group {thing_group_name} does not exist.")
}

/// The sentence the operations addressing the GROUP itself state the same miss with: the
/// registry families' account-qualified form, which is the other half of that family.
fn registry_group_missing_message(thing_group_name: &str) -> String {
    format!("ThingGroup {ACCOUNT_ID}:{thing_group_name} not found.")
}

/// `thingName`s spanning the modelled class (`ThingName`, length 1..128, pattern
/// `^[a-zA-Z0-9:_-]+$`): the shortest, ordinary names of each punctuation the pattern
/// admits, and the longest.
fn missing_thing_names() -> Vec<String> {
    vec![
        "a".to_string(),
        "ghost-thing".to_string(),
        "Ghost_Thing-42".to_string(),
        "sensor:east:1".to_string(),
        "n".repeat(128),
    ]
}

#[tokio::test]
async fn a_missing_thing_is_refused_in_the_wording_of_the_call_that_failed() {
    let emulator = Emulator::new();

    // The principal is echoed into the sentence whichever form it is named in and whether
    // or not the account holds it, because the refusal is about the THING: a certificate
    // this account really holds, one it does not, and an Amazon Cognito identity.
    let held_certificate = emulator.given_certificate().await;
    let unheld_certificate =
        format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:cert/{}", "0".repeat(64));
    let principals = [
        held_certificate,
        unheld_certificate,
        COGNITO_IDENTITY.to_string(),
    ];

    for thing_name in missing_thing_names() {
        for principal in &principals {
            // The thing is the resource both writes ADDRESS, so a name the registry does not
            // hold is answered before the principal is resolved at all — which is what the
            // unheld certificate row pins.
            let attached = emulator.attach(&thing_name, principal).await;
            assert_eq!(
                (
                    attached.status,
                    attached.error_code.clone(),
                    attached.message()
                ),
                (
                    404,
                    Some(NOT_FOUND.to_string()),
                    Some(attach_missing_thing_message(principal, &thing_name))
                ),
                "case attach {principal:?} to the missing thing {thing_name:?}: expected 404 \
                 {:?}, got {attached:?}",
                attach_missing_thing_message(principal, &thing_name)
            );

            let detached = emulator.detach(&thing_name, principal).await;
            assert_eq!(
                (
                    detached.status,
                    detached.error_code.clone(),
                    detached.message()
                ),
                (
                    404,
                    Some(NOT_FOUND.to_string()),
                    Some(detach_missing_thing_message(principal, &thing_name))
                ),
                "case detach {principal:?} from the missing thing {thing_name:?}: expected \
                 404 {:?}, got {detached:?}",
                detach_missing_thing_message(principal, &thing_name)
            );
        }

        // The listing is OF the thing, so it states the same miss in its own sentence —
        // never an empty page.
        let listed = emulator.list_principals(&thing_name).await;
        assert_eq!(
            (listed.status, listed.error_code.clone(), listed.message()),
            (
                404,
                Some(NOT_FOUND.to_string()),
                Some(list_principals_missing_thing_message(&thing_name))
            ),
            "case list the principals of the missing thing {thing_name:?}: expected 404 {:?}, \
             got {listed:?}",
            list_principals_missing_thing_message(&thing_name)
        );
    }
}

#[tokio::test]
async fn a_principal_is_classified_before_it_is_resolved() {
    let emulator = Emulator::new();

    let thing_name = "principal-classification";
    emulator.given_thing(thing_name).await;
    // A policy the account really holds, whose ARN is still no principal of a thing: what
    // makes an address a principal is the resource it names, not whether it exists.
    let policy_name = "device-publish";
    emulator.given_policy(policy_name).await;
    emulator.given_group("factory").await;

    // Values that are no principal address at all: not an ARN and not the `<region>:<id>`
    // form a Cognito identity takes. Each is the modelled 400, naming the type that could
    // not be read and echoing the value verbatim.
    let no_address = [
        "not-an-arn".to_string(),
        // A bare certificate id — the id inside a certificate ARN, sent without the ARN
        // around it.
        "f".repeat(64),
        "arn".to_string(),
        "cert/0123456789".to_string(),
        // Too few fields to be read as an ARN by any service.
        format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}"),
        // The `<region>:<id>` form with no id: no Cognito identity, and no ARN either.
        format!("{REGION}:"),
        // An ARN namespace is a fixed lowercase token, so this opens with no `arn:` at all.
        format!("ARN:aws:iot:{REGION}:{ACCOUNT_ID}:cert/{}", "f".repeat(64)),
    ];
    for principal in &no_address {
        let attached = emulator.attach(thing_name, principal).await;
        assert_eq!(
            (
                attached.status,
                attached.error_code.clone(),
                attached.message()
            ),
            (
                400,
                Some(INVALID_REQUEST.to_string()),
                Some(invalid_principal_type_message(principal))
            ),
            "case attach {principal:?}, which is no principal address: expected 400 {:?}, got \
             {attached:?}",
            invalid_principal_type_message(principal)
        );

        let detached = emulator.detach(thing_name, principal).await;
        assert_eq!(
            (
                detached.status,
                detached.error_code.clone(),
                detached.message()
            ),
            (
                400,
                Some(INVALID_REQUEST.to_string()),
                Some(invalid_principal_type_message(principal))
            ),
            "case detach {principal:?}, which is no principal address: expected 400 {:?}, got \
             {detached:?}",
            invalid_principal_type_message(principal)
        );
    }

    // Well-formed addresses this account holds no PRINCIPAL under: a certificate it does
    // not hold, resources of five other types — one of which (the policy) it really holds —
    // and one of another partition. Each is the modelled 404 and not a syntax refusal,
    // because the value addresses something; a principal under it could be created.
    let unresolved = [
        format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:cert/{}", "0".repeat(64)),
        format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:policy/{policy_name}"),
        format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thinggroup/factory"),
        format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{thing_name}"),
        format!("arn:aws:iam::{ACCOUNT_ID}:role/device-role"),
        format!(
            "arn:aws-us-gov:iot:us-gov-west-1:{ACCOUNT_ID}:cert/{}",
            "a".repeat(64)
        ),
    ];
    for principal in &unresolved {
        let attached = emulator.attach(thing_name, principal).await;
        assert_eq!(
            (
                attached.status,
                attached.error_code.clone(),
                attached.message()
            ),
            (
                404,
                Some(NOT_FOUND.to_string()),
                Some(attach_missing_principal_message(principal, thing_name))
            ),
            "case attach {principal:?}, which names no principal of this account: expected \
             404 {:?}, got {attached:?}",
            attach_missing_principal_message(principal, thing_name)
        );

        let detached = emulator.detach(thing_name, principal).await;
        assert_eq!(
            (
                detached.status,
                detached.error_code.clone(),
                detached.message()
            ),
            (
                404,
                Some(NOT_FOUND.to_string()),
                Some(detach_missing_principal_message(principal, thing_name))
            ),
            "case detach {principal:?}, which names no principal of this account: expected \
             404 {:?}, got {detached:?}",
            detach_missing_principal_message(principal, thing_name)
        );
    }

    // The two forms the relation DOES take are attached and reported, so the refusals above
    // are about what the value names and not a validator that refuses everything.
    let certificate_arn = emulator.given_certificate().await;
    let mut attached_principals = vec![certificate_arn.clone(), COGNITO_IDENTITY.to_string()];
    for principal in &attached_principals {
        let attached = emulator.attach(thing_name, principal).await;
        assert_eq!(
            (attached.status, attached.message()),
            (200, None),
            "case attach the resolvable principal {principal:?}: expected an empty 200, got \
             {attached:?}"
        );
    }
    attached_principals.sort();
    let listed = emulator.list_principals(thing_name).await;
    assert_eq!(
        (listed.status, listed.principals()),
        (200, attached_principals.clone()),
        "case list the attached principals: expected {attached_principals:?}, got {listed:?}"
    );

    for principal in &attached_principals {
        let detached = emulator.detach(thing_name, principal).await;
        assert_eq!(
            (detached.status, detached.message()),
            (200, None),
            "case detach the resolvable principal {principal:?}: expected an empty 200, got \
             {detached:?}"
        );
    }
    let emptied = emulator.list_principals(thing_name).await;
    assert_eq!(
        (emptied.status, emptied.principals()),
        (200, Vec::new()),
        "case list after detaching both: expected an empty page, got {emptied:?}"
    );
}

#[tokio::test]
async fn the_tag_plane_states_a_missing_thing_group_in_its_own_words() {
    let emulator = Emulator::new();

    // `ThingGroupName` is length 1..128, pattern `^[a-zA-Z0-9:_-]+$`: the shortest name, an
    // ordinary one, one of each punctuation the pattern admits, and the longest.
    let missing_groups = [
        "a".to_string(),
        "fleet-alpha".to_string(),
        "Fleet_Group:2".to_string(),
        "g".repeat(128),
    ];
    for thing_group_name in &missing_groups {
        let arn = format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thinggroup/{thing_group_name}");
        let expected = tagged_group_missing_message(thing_group_name);

        for (operation, reply) in [
            ("TagResource", emulator.tag(&arn, "stage", "test").await),
            ("UntagResource", emulator.untag(&arn, "stage").await),
        ] {
            assert_eq!(
                (reply.status, reply.error_code.clone(), reply.message()),
                (404, Some(NOT_FOUND.to_string()), Some(expected.clone())),
                "case {operation} on the missing group {thing_group_name:?}: expected 404 \
                 {expected:?}, got {reply:?}"
            );
        }

        // The tag READ of an ARN no resource answers to is SERVED and not refused — the
        // recorded behaviour this service already implements (`ops::list_tags_for_resource`)
        // — so the sentence above belongs to the two WRITES, and this row pins that the
        // change did not turn the read into a refusal.
        let read = emulator.list_tags(&arn).await;
        assert_eq!(
            (read.status, read.tag_keys()),
            (200, Vec::new()),
            "case ListTagsForResource on the missing group {thing_group_name:?}: expected an \
             empty 200, got {read:?}"
        );

        // The other half of this family: an operation addressing the GROUP states the same
        // miss in the registry families' account-qualified sentence, so the two planes are
        // pinned apart here rather than collapsed into one wording.
        let described = emulator.describe_group(thing_group_name).await;
        assert_eq!(
            (described.status, described.message()),
            (
                404,
                Some(registry_group_missing_message(thing_group_name))
            ),
            "case DescribeThingGroup on the missing group {thing_group_name:?}: expected 404 \
             {:?}, got {described:?}",
            registry_group_missing_message(thing_group_name)
        );
    }

    // A group the account DOES hold takes the write and reports it back, so the refusals
    // above are the lookup's and not the plane's.
    let thing_group_name = "fleet-alpha";
    emulator.given_group(thing_group_name).await;
    let arn = format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thinggroup/{thing_group_name}");
    let tagged = emulator.tag(&arn, "stage", "test").await;
    assert_eq!(
        tagged.status, 200,
        "case TagResource on the held group {thing_group_name:?}: expected 200, got {tagged:?}"
    );
    let read = emulator.list_tags(&arn).await;
    assert_eq!(
        (read.status, read.tag_keys()),
        (200, vec!["stage".to_string()]),
        "case ListTagsForResource on the held group {thing_group_name:?}: expected the written \
         tag, got {read:?}"
    );
    let untagged = emulator.untag(&arn, "stage").await;
    assert_eq!(
        untagged.status, 200,
        "case UntagResource on the held group {thing_group_name:?}: expected 200, got \
         {untagged:?}"
    );
    let emptied = emulator.list_tags(&arn).await;
    assert_eq!(
        (emptied.status, emptied.tag_keys()),
        (200, Vec::new()),
        "case ListTagsForResource after the untag: expected no tags, got {emptied:?}"
    );
}
