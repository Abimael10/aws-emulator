// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the DELETES the IoT registry gates on a dependent record,
//! and of what a delete that does take effect removes.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rule under test: a delete of a registry resource takes effect only while nothing in
//! the account's own state depends on the record — no principal attached to the thing, no
//! thing attached to the certificate, no child group under the thing group, and the
//! `expectedVersion` the caller named matching the record — and a refused delete is
//! answered with the modelled class for what held it back, leaving the record exactly as it
//! was; a delete that is NOT held back removes the record from the state every later reader
//! consults, so the reader of that name refuses afterwards.
//!
//! The tables span what the rule generalises over rather than one recorded request: both
//! forms a thing principal is named in (a certificate ARN and an Amazon Cognito identity),
//! several counts of attached principals, every form `thingPrincipalType` takes on the
//! wire, groups at several places in the hierarchy, several counts of child groups, and
//! several versions a group record can be at.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`), and
/// the account the registry families' not-found sentences are qualified with.
const ACCOUNT_ID: &str = "000000000000";

/// The region the harness signs for.
const REGION: &str = "us-east-1";

/// An IoT policy document every policy in this suite is created with: the document is not
/// what this suite is about, so one valid document serves every case.
const POLICY_DOCUMENT: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"iot:*","Resource":"*"}]}"#;

/// Amazon Cognito identities of the `<region>:<id>` form `ListAttachedPolicies$target`
/// documents for a principal ("CognitoId (region:id)",
/// `smithy_query(query="operation",operation="ListAttachedPolicies")`). They are principals
/// no record of this account has to exist for — identity pools live in another service — so
/// a case needing several principals uses several of these.
const COGNITO_IDENTITIES: [&str; 3] = [
    "us-east-1:11111111-1111-1111-1111-111111111111",
    "us-east-1:22222222-2222-2222-2222-222222222222",
    "eu-west-1:33333333-3333-3333-3333-333333333333",
];

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name and
/// the region: the scope is how the router picks the claiming service, and how the service
/// picks the region its ARNs are scoped to. The signature itself is never checked here —
/// the harness turns request authentication off, exactly as an unauthenticated emulator
/// runs.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// A string member of the body, or `None` when it is absent or not a string.
    fn text(&self, name: &str) -> Option<String> {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The `message` of an error body.
    fn message(&self) -> String {
        self.text("message").unwrap_or_default()
    }

    /// The `principals` of a `ListThingPrincipals` answer, in the order it reported them.
    fn principals(&self) -> Vec<String> {
        self.body
            .get("principals")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| entry.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The `version` a thing-group read reports.
    fn version(&self) -> Option<i64> {
        self.body.get("version").and_then(Value::as_i64)
    }

    /// The `thingGroupDescription` of a reported `thingGroupProperties`.
    fn group_description(&self) -> Option<String> {
        self.body
            .get("thingGroupProperties")
            .and_then(|properties| properties.get("thingGroupDescription"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// Whether the body carries no member at all — what an operation whose output shape is
    /// `Unit` answers with.
    fn is_empty_body(&self) -> bool {
        match &self.body {
            Value::Null => true,
            Value::Object(members) => members.is_empty(),
            _ => false,
        }
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
            .header("authorization", signed_for_iot())
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

    /// `CreateThing`: `POST /things/{thingName}`.
    async fn create_thing(&self, thing_name: &str) -> Reply {
        self.call("POST", &format!("/things/{thing_name}"), Some(&json!({})), &[])
            .await
    }

    /// `DescribeThing`: `GET /things/{thingName}`.
    async fn describe_thing(&self, thing_name: &str) -> Reply {
        self.call("GET", &format!("/things/{thing_name}"), None, &[])
            .await
    }

    /// `DeleteThing`: `DELETE /things/{thingName}`.
    async fn delete_thing(&self, thing_name: &str) -> Reply {
        self.call("DELETE", &format!("/things/{thing_name}"), None, &[])
            .await
    }

    /// `AttachThingPrincipal`: `PUT /things/{thingName}/principals`, with the principal in
    /// the header the model binds it to and `thingPrincipalType` as a query parameter —
    /// absent when the case names none.
    async fn attach_principal(
        &self,
        thing_name: &str,
        principal: &str,
        principal_type: Option<&str>,
    ) -> Reply {
        let path = match principal_type {
            None => format!("/things/{thing_name}/principals"),
            Some(named) => {
                format!("/things/{thing_name}/principals?thingPrincipalType={named}")
            }
        };
        self.call("PUT", &path, None, &[("x-amzn-principal", principal)])
            .await
    }

    /// `DetachThingPrincipal`: `DELETE /things/{thingName}/principals`.
    async fn detach_principal(&self, thing_name: &str, principal: &str) -> Reply {
        self.call(
            "DELETE",
            &format!("/things/{thing_name}/principals"),
            None,
            &[("x-amzn-principal", principal)],
        )
        .await
    }

    /// `ListThingPrincipals`: `GET /things/{thingName}/principals`, with the page the case
    /// asked for.
    async fn list_principals(
        &self,
        thing_name: &str,
        max_results: Option<i32>,
        next_token: Option<&str>,
    ) -> Reply {
        let mut query: Vec<String> = Vec::new();
        if let Some(max) = max_results {
            query.push(format!("maxResults={max}"));
        }
        if let Some(token) = next_token {
            query.push(format!("nextToken={token}"));
        }
        let path = if query.is_empty() {
            format!("/things/{thing_name}/principals")
        } else {
            format!("/things/{thing_name}/principals?{}", query.join("&"))
        };
        self.call("GET", &path, None, &[]).await
    }

    /// `CreateKeysAndCertificate`: `POST /keys-and-certificate`, which issues the
    /// certificate a device is reached through.
    async fn create_certificate(&self, set_as_active: bool) -> Reply {
        self.call(
            "POST",
            &format!("/keys-and-certificate?setAsActive={set_as_active}"),
            None,
            &[],
        )
        .await
    }

    /// `DeleteCertificate`: `DELETE /certificates/{certificateId}`, with `forceDelete` bound
    /// as an `@httpQuery` parameter — absent when the case names none.
    async fn delete_certificate(&self, certificate_id: &str, force: Option<bool>) -> Reply {
        let path = match force {
            None => format!("/certificates/{certificate_id}"),
            Some(force) => format!("/certificates/{certificate_id}?forceDelete={force}"),
        };
        self.call("DELETE", &path, None, &[]).await
    }

    /// `DescribeCertificate`: `GET /certificates/{certificateId}`.
    async fn describe_certificate(&self, certificate_id: &str) -> Reply {
        self.call("GET", &format!("/certificates/{certificate_id}"), None, &[])
            .await
    }

    /// `CreatePolicy`: `POST /policies/{policyName}`.
    async fn create_policy(&self, policy_name: &str) -> Reply {
        self.call(
            "POST",
            &format!("/policies/{policy_name}"),
            Some(&json!({ "policyDocument": POLICY_DOCUMENT })),
            &[],
        )
        .await
    }

    /// `AttachPolicy`: `PUT /target-policies/{policyName}` carrying the target.
    async fn attach_policy(&self, policy_name: &str, target: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/target-policies/{policy_name}"),
            Some(&json!({ "target": target })),
            &[],
        )
        .await
    }

    /// `CreateThingGroup`: `POST /thing-groups/{thingGroupName}`.
    async fn create_group(&self, thing_group_name: &str, request: &Value) -> Reply {
        self.call(
            "POST",
            &format!("/thing-groups/{thing_group_name}"),
            Some(request),
            &[],
        )
        .await
    }

    /// `CreateDynamicThingGroup`: `POST /dynamic-thing-groups/{thingGroupName}`, the other
    /// kind of group — the one deleted through `DeleteDynamicThingGroup`.
    async fn create_dynamic_group(&self, thing_group_name: &str, request: &Value) -> Reply {
        self.call(
            "POST",
            &format!("/dynamic-thing-groups/{thing_group_name}"),
            Some(request),
            &[],
        )
        .await
    }

    /// `DeleteDynamicThingGroup`: `DELETE /dynamic-thing-groups/{thingGroupName}`.
    async fn delete_dynamic_group(&self, thing_group_name: &str) -> Reply {
        self.call(
            "DELETE",
            &format!("/dynamic-thing-groups/{thing_group_name}"),
            None,
            &[],
        )
        .await
    }

    /// `UpdateThingGroup`: `PATCH /thing-groups/{thingGroupName}`.
    async fn update_group(&self, thing_group_name: &str, request: &Value) -> Reply {
        self.call(
            "PATCH",
            &format!("/thing-groups/{thing_group_name}"),
            Some(request),
            &[],
        )
        .await
    }

    /// `DescribeThingGroup`: `GET /thing-groups/{thingGroupName}`.
    async fn describe_group(&self, thing_group_name: &str) -> Reply {
        self.call("GET", &format!("/thing-groups/{thing_group_name}"), None, &[])
            .await
    }

    /// `DeleteThingGroup`: `DELETE /thing-groups/{thingGroupName}`, with `expectedVersion`
    /// bound as an `@httpQuery` parameter — absent when the case names none.
    async fn delete_group(&self, thing_group_name: &str, expected_version: Option<i64>) -> Reply {
        let path = match expected_version {
            None => format!("/thing-groups/{thing_group_name}"),
            Some(version) => {
                format!("/thing-groups/{thing_group_name}?expectedVersion={version}")
            }
        };
        self.call("DELETE", &path, None, &[]).await
    }

    /// A thing created for a case.
    async fn thing(&self, case: &str, thing_name: &str) {
        let created = self.create_thing(thing_name).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateThing {thing_name} must succeed, got {created:?}"
        );
    }

    /// A certificate issued for a case, as the pair every assertion needs: its id and its
    /// ARN.
    async fn certificate(&self, case: &str, set_as_active: bool) -> (String, String) {
        let issued = self.create_certificate(set_as_active).await;
        assert_eq!(
            issued.status, 200,
            "case {case:?}: CreateKeysAndCertificate must succeed, got {issued:?}"
        );
        let id = issued.text("certificateId").unwrap_or_else(|| {
            panic!("case {case:?}: the issue must report a certificateId, got {issued:?}")
        });
        let arn = issued.text("certificateArn").unwrap_or_else(|| {
            panic!("case {case:?}: the issue must report a certificateArn, got {issued:?}")
        });
        (id, arn)
    }
}

/// The message AWS IoT refuses the delete of a thing that still has a principal attached
/// with: fixed text, with the thing name taken from the request.
fn attached_principals_message(thing_name: &str) -> String {
    format!("Cannot delete. Thing {thing_name} is still attached to one or more principals")
}

/// The message AWS IoT refuses the delete of a certificate an IoT thing still has attached
/// with: fixed text, with the ARN taken from the stored certificate.
fn attached_things_message(certificate_arn: &str) -> String {
    format!("Things must be detached before deletion (arn: {certificate_arn})")
}

/// The message AWS IoT refuses the delete of a group that still has child groups with:
/// fixed text (the space before the colon included), with the parent's name from the
/// request.
fn child_groups_message(thing_group_name: &str) -> String {
    format!(
        "Cannot delete thing group : {thing_group_name} when there are still child groups \
         attached to it"
    )
}

/// The sentence the thing-group family states a missing group with: the family noun, the
/// account the record would belong to, and the name the request asked about.
fn missing_group_message(thing_group_name: &str) -> String {
    format!("ThingGroup {ACCOUNT_ID}:{thing_group_name} not found.")
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

/// The sentence this family states a thing-group version conflict with: the group the
/// request addressed is the only variable part.
fn group_version_conflict_message(thing_group_name: &str) -> String {
    format!("The version for thing group {thing_group_name} does not match the expected version.")
}

/// The message this service refuses the delete of a DYNAMIC group through
/// `DeleteThingGroup` with: the mirror of what `DeleteDynamicThingGroup` answers for a
/// static one.
fn dynamic_group_message(thing_group_name: &str) -> String {
    format!("Thing group {thing_group_name} is a dynamic thing group")
}

/// The forms `thingPrincipalType` takes on the wire: absent (the documented default
/// `NON_EXCLUSIVE_THING`, `smithy_query(query="shape",shape="ThingPrincipalObject")`) and
/// each of the two modelled values.
fn principal_type_forms() -> Vec<(&'static str, Option<&'static str>)> {
    vec![
        ("no thingPrincipalType at all", None),
        ("thingPrincipalType=NON_EXCLUSIVE_THING", Some("NON_EXCLUSIVE_THING")),
        ("thingPrincipalType=EXCLUSIVE_THING", Some("EXCLUSIVE_THING")),
    ]
}

#[tokio::test]
async fn an_attached_principal_is_reported_and_a_detach_removes_it() {
    let emulator = Emulator::new();

    // The attach is observable through `ListThingPrincipals` whichever form the principal is
    // named in and whichever relation type the request asked for, the detach removes exactly
    // the one principal it names, and both are idempotent: repeating either leaves the same
    // relation.
    let (_certificate_id, certificate_arn) = emulator.certificate("principal forms", true).await;
    let forms: Vec<(&str, String)> = vec![
        ("a certificate ARN", certificate_arn.clone()),
        ("an Amazon Cognito identity", COGNITO_IDENTITIES[0].to_string()),
    ];
    for (form, principal) in &forms {
        for (type_form, principal_type) in principal_type_forms() {
            let case = format!("{form} with {type_form}");
            let thing_name = format!("thing-{}-{}", forms.len(), case.len());
            emulator.thing(&case, &thing_name).await;

            let attached = emulator
                .attach_principal(&thing_name, principal, principal_type)
                .await;
            assert_eq!(
                attached.status, 200,
                "case {case:?}: AttachThingPrincipal must succeed, got {attached:?}"
            );
            assert!(
                attached.is_empty_body(),
                "case {case:?}: AttachThingPrincipal models no output members, got {attached:?}"
            );

            let listed = emulator.list_principals(&thing_name, None, None).await;
            assert_eq!(
                listed.principals(),
                vec![principal.clone()],
                "case {case:?}: the attached principal must be reported, got {listed:?}"
            );

            // Attaching the same principal again names no new identity, so the relation
            // still holds the one attachment.
            let again = emulator
                .attach_principal(&thing_name, principal, principal_type)
                .await;
            assert_eq!(
                again.status, 200,
                "case {case:?}: a repeated attach must succeed, got {again:?}"
            );
            let listed = emulator.list_principals(&thing_name, None, None).await;
            assert_eq!(
                listed.principals(),
                vec![principal.clone()],
                "case {case:?}: a repeated attach must leave one attachment, got {listed:?}"
            );

            let detached = emulator.detach_principal(&thing_name, principal).await;
            assert_eq!(
                detached.status, 200,
                "case {case:?}: DetachThingPrincipal must succeed, got {detached:?}"
            );
            let listed = emulator.list_principals(&thing_name, None, None).await;
            assert!(
                listed.principals().is_empty(),
                "case {case:?}: the detached principal must be gone, got {listed:?}"
            );

            // A principal that is not attached removes nothing and still succeeds.
            let repeat = emulator.detach_principal(&thing_name, principal).await;
            assert_eq!(
                repeat.status, 200,
                "case {case:?}: a repeated detach must succeed, got {repeat:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_listing_of_a_things_principals_pages_and_refuses_an_unregistered_thing() {
    let emulator = Emulator::new();

    // Every principal the thing holds is reported, in one page when the request asks for no
    // size and one page at a time when it does — and `nextToken` is present exactly while
    // the listing is truncated.
    let thing_name = "paged-principals";
    emulator.thing("paging", thing_name).await;
    for identity in COGNITO_IDENTITIES {
        let attached = emulator.attach_principal(thing_name, identity, None).await;
        assert_eq!(
            attached.status, 200,
            "case \"paging\": attaching {identity} must succeed, got {attached:?}"
        );
    }

    let whole = emulator.list_principals(thing_name, None, None).await;
    let mut expected: Vec<String> = COGNITO_IDENTITIES.iter().map(|id| id.to_string()).collect();
    expected.sort();
    assert_eq!(
        whole.principals(),
        expected,
        "case \"paging\": one page must report every principal, got {whole:?}"
    );
    assert!(
        whole.text("nextToken").is_none(),
        "case \"paging\": an untruncated listing carries no nextToken, got {whole:?}"
    );

    for page_size in [1i32, 2] {
        let case = format!("maxResults={page_size}");
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        // Bounded by one request per principal plus one: a listing that asked for a page
        // cannot need more pages than it has rows.
        for _ in 0..=COGNITO_IDENTITIES.len() {
            let page = emulator
                .list_principals(thing_name, Some(page_size), token.as_deref())
                .await;
            assert_eq!(
                page.status, 200,
                "case {case:?}: every page must be answered, got {page:?}"
            );
            assert!(
                page.principals().len() <= page_size as usize,
                "case {case:?}: a page must carry at most {page_size} rows, got {page:?}"
            );
            walked.extend(page.principals());
            token = page.text("nextToken");
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            walked, expected,
            "case {case:?}: walking the pages must report every principal once, got {walked:?}"
        );
    }

    // A token this listing did not issue selects no page of it.
    let refused = emulator
        .list_principals(thing_name, None, Some("6e6f742d612d746f6b656e"))
        .await;
    assert_eq!(
        refused.status, 400,
        "case \"a foreign nextToken\": the listing must refuse it, got {refused:?}"
    );

    // The listing is OF a thing, so a name the registry does not hold is a 404 and never an
    // empty page.
    for unregistered in ["never-created", "paged-principals-2"] {
        let missing = emulator.list_principals(unregistered, None, None).await;
        assert_eq!(
            missing.status, 404,
            "case {unregistered:?}: listing an unregistered thing must be refused 404, \
             got {missing:?}"
        );
        assert_eq!(
            missing.error_code.as_deref(),
            Some("ResourceNotFoundException"),
            "case {unregistered:?}: expected ResourceNotFoundException, got {missing:?}"
        );
        assert_eq!(
            missing.message(),
            list_principals_missing_thing_message(unregistered),
            "case {unregistered:?}: expected {:?}, got {missing:?}",
            list_principals_missing_thing_message(unregistered)
        );
    }
}

#[tokio::test]
async fn attaching_a_principal_nothing_holds_is_refused() {
    let emulator = Emulator::new();

    let thing_name = "attach-refusals";
    emulator.thing("attach refusals", thing_name).await;

    // A well-formed certificate ARN the account holds no certificate under is the modelled
    // 404: the principal names nothing to attach.
    for certificate_id in ["0".repeat(64), "f".repeat(64)] {
        let principal = format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:cert/{certificate_id}");
        let refused = emulator.attach_principal(thing_name, &principal, None).await;
        assert_eq!(
            refused.status, 404,
            "case {principal:?}: attaching an unheld certificate must be refused 404, \
             got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("ResourceNotFoundException"),
            "case {principal:?}: expected ResourceNotFoundException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            attach_missing_principal_message(&principal, thing_name),
            "case {principal:?}: expected {:?}, got {refused:?}",
            attach_missing_principal_message(&principal, thing_name)
        );
    }

    // An ARN of any OTHER resource addresses something too, and this account holds no
    // principal under it either, so it is the same modelled 404 and not a syntax refusal.
    for principal in [
        format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thinggroup/factory"),
        format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{thing_name}"),
    ] {
        let refused = emulator.attach_principal(thing_name, &principal, None).await;
        assert_eq!(
            (refused.status, refused.message()),
            (
                404,
                attach_missing_principal_message(&principal, thing_name)
            ),
            "case {principal:?}: a well-formed address naming no principal must be refused \
             404, got {refused:?}"
        );
    }

    // A value that is no principal address at all names no principal in any account, so it
    // is the modelled 400.
    for principal in ["not-an-arn".to_string(), "f".repeat(64)] {
        let refused = emulator.attach_principal(thing_name, &principal, None).await;
        assert_eq!(
            (refused.status, refused.message()),
            (400, format!("Invalid principal type: {principal}")),
            "case {principal:?}: attaching a value that is no principal address must be \
             refused 400, got {refused:?}"
        );
    }

    // The thing is the resource the attach addresses, so a name the registry does not hold is
    // the modelled 404 whichever principal the request carries.
    let missing = emulator
        .attach_principal("never-created", COGNITO_IDENTITIES[0], None)
        .await;
    assert_eq!(
        missing.status, 404,
        "case \"an unregistered thing\": the attach must be refused 404, got {missing:?}"
    );
    assert_eq!(
        missing.message(),
        attach_missing_thing_message(COGNITO_IDENTITIES[0], "never-created"),
        "case \"an unregistered thing\": expected {:?}, got {missing:?}",
        attach_missing_thing_message(COGNITO_IDENTITIES[0], "never-created")
    );

    // Nothing above was recorded: the thing still holds no principal.
    let listed = emulator.list_principals(thing_name, None, None).await;
    assert!(
        listed.principals().is_empty(),
        "a refused attach must record nothing, got {listed:?}"
    );
}

#[tokio::test]
async fn deleting_a_thing_with_an_attached_principal_is_refused_and_deletes_nothing() {
    let emulator = Emulator::new();

    // AWS IoT makes detach-before-delete mandatory: while the thing holds ANY principal the
    // delete is refused and the thing stays readable, and once every principal is detached
    // the same delete takes effect.
    for count in [1usize, 2, 3] {
        let case = format!("{count} attached principals");
        let thing_name = format!("gated-thing-{count}");
        emulator.thing(&case, &thing_name).await;
        let principals: Vec<&str> = COGNITO_IDENTITIES.iter().copied().take(count).collect();
        for principal in &principals {
            let attached = emulator.attach_principal(&thing_name, principal, None).await;
            assert_eq!(
                attached.status, 200,
                "case {case:?}: attaching {principal} must succeed, got {attached:?}"
            );
        }

        let refused = emulator.delete_thing(&thing_name).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: an attached principal must refuse the delete 400, got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/"),
            "case {case:?}: expected the namespaced InvalidRequestException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            attached_principals_message(&thing_name),
            "case {case:?}: expected {:?}, got {refused:?}",
            attached_principals_message(&thing_name)
        );

        // The refusal wrote nothing: the thing is still readable and every attachment is
        // still recorded.
        let read = emulator.describe_thing(&thing_name).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: a refused delete must leave the thing readable, got {read:?}"
        );
        let listed = emulator.list_principals(&thing_name, None, None).await;
        assert_eq!(
            listed.principals().len(),
            count,
            "case {case:?}: a refused delete must detach nothing, got {listed:?}"
        );

        // Detaching all but the last one leaves the delete refused: it is "one or more".
        for principal in &principals[..count - 1] {
            let detached = emulator.detach_principal(&thing_name, principal).await;
            assert_eq!(
                detached.status, 200,
                "case {case:?}: detaching {principal} must succeed, got {detached:?}"
            );
            let still = emulator.delete_thing(&thing_name).await;
            assert_eq!(
                still.status, 400,
                "case {case:?}: one remaining principal must still refuse the delete, \
                 got {still:?}"
            );
        }

        let detached = emulator
            .detach_principal(&thing_name, principals[count - 1])
            .await;
        assert_eq!(
            detached.status, 200,
            "case {case:?}: the last detach must succeed, got {detached:?}"
        );

        let deleted = emulator.delete_thing(&thing_name).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: a detached thing must delete, got {deleted:?}"
        );
        assert!(
            deleted.is_empty_body(),
            "case {case:?}: DeleteThing models no output members, got {deleted:?}"
        );
        let gone = emulator.describe_thing(&thing_name).await;
        assert_eq!(
            gone.status, 404,
            "case {case:?}: the deleted thing must be gone, got {gone:?}"
        );
    }
}

#[tokio::test]
async fn deleting_a_certificate_a_thing_still_holds_is_refused_naming_the_thing_attachment() {
    let emulator = Emulator::new();

    // "A certificate cannot be deleted if it has a policy or IoT thing attached to it"
    // (`smithy_query(query="operation",operation="DeleteCertificate")`), and `forceDelete`
    // forces the deletion only of a certificate that "is not attached to an IoT thing", so
    // the thing attachment refuses the delete whichever form `forceDelete` takes — and the
    // refusal names the thing attachment even when a policy is attached as well, because
    // that is the relation the caller has to clear.
    for (case, with_policy) in [
        ("a thing attached", false),
        ("a thing and a policy attached", true),
    ] {
        let (certificate_id, certificate_arn) = emulator.certificate(case, false).await;
        let thing_name = format!("certificate-holder-{}", &certificate_id[..8]);
        emulator.thing(case, &thing_name).await;
        let attached = emulator
            .attach_principal(&thing_name, &certificate_arn, None)
            .await;
        assert_eq!(
            attached.status, 200,
            "case {case:?}: attaching the certificate must succeed, got {attached:?}"
        );
        if with_policy {
            let policy_name = format!("device-policy-{}", &certificate_id[..8]);
            let created = emulator.create_policy(&policy_name).await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: CreatePolicy must succeed, got {created:?}"
            );
            let attached = emulator.attach_policy(&policy_name, &certificate_arn).await;
            assert_eq!(
                attached.status, 200,
                "case {case:?}: AttachPolicy must succeed, got {attached:?}"
            );
        }

        for force in [None, Some(false), Some(true)] {
            let refused = emulator.delete_certificate(&certificate_id, force).await;
            assert_eq!(
                refused.status, 409,
                "case {case:?} with forceDelete={force:?}: the attached thing must refuse the \
                 delete 409, got {refused:?}"
            );
            assert_eq!(
                refused.error_code.as_deref(),
                Some("DeleteConflictException"),
                "case {case:?} with forceDelete={force:?}: expected DeleteConflictException, \
                 got {refused:?}"
            );
            assert_eq!(
                refused.message(),
                attached_things_message(&certificate_arn),
                "case {case:?} with forceDelete={force:?}: expected {:?}, got {refused:?}",
                attached_things_message(&certificate_arn)
            );
            let read = emulator.describe_certificate(&certificate_id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: a refused delete must leave the certificate readable, \
                 got {read:?}"
            );
        }

        // Once the thing is detached the certificate deletes — forced, because the policy of
        // the second case is still attached and that is the conflict the force relaxes.
        let detached = emulator
            .detach_principal(&thing_name, &certificate_arn)
            .await;
        assert_eq!(
            detached.status, 200,
            "case {case:?}: DetachThingPrincipal must succeed, got {detached:?}"
        );
        let deleted = emulator.delete_certificate(&certificate_id, Some(true)).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: a detached INACTIVE certificate must delete, got {deleted:?}"
        );
        let gone = emulator.describe_certificate(&certificate_id).await;
        assert_eq!(
            gone.status, 404,
            "case {case:?}: the deleted certificate must be gone, got {gone:?}"
        );
    }
}

#[tokio::test]
async fn deleting_a_thing_group_removes_it_from_every_reader() {
    let emulator = Emulator::new();

    // A group nothing depends on is removed, whatever it carries and wherever it sits in the
    // hierarchy: the delete answers an empty 200, the describe path refuses with the
    // thing-group family's own not-found sentence, and the delete is idempotent because the
    // operation models no not-found at all.
    let parent = "hierarchy-parent";
    let created = emulator.create_group(parent, &json!({})).await;
    assert_eq!(
        created.status, 200,
        "case \"the parent group\": CreateThingGroup must succeed, got {created:?}"
    );

    let cases: Vec<(&str, Value)> = vec![
        ("a root group with no properties", json!({})),
        (
            "a group carrying a description",
            json!({ "thingGroupProperties": { "thingGroupDescription": "line of sensors" } }),
        ),
        (
            "a group carrying attributes",
            json!({
                "thingGroupProperties": {
                    "attributePayload": { "attributes": { "floor": "3", "line": "a" } }
                }
            }),
        ),
        (
            "a child group under a live parent",
            json!({ "parentGroupName": parent }),
        ),
    ];
    for (case, request) in cases {
        let group_name = format!("deleted-group-{}", case.len());
        let created = emulator.create_group(&group_name, &request).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateThingGroup must succeed, got {created:?}"
        );
        let read = emulator.describe_group(&group_name).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the group under test must be readable first, got {read:?}"
        );

        let deleted = emulator.delete_group(&group_name, None).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: deleting a group nothing depends on must succeed, got {deleted:?}"
        );
        assert!(
            deleted.is_empty_body(),
            "case {case:?}: DeleteThingGroup models no output members, got {deleted:?}"
        );

        let gone = emulator.describe_group(&group_name).await;
        assert_eq!(
            gone.status, 404,
            "case {case:?}: the deleted group must be gone, got {gone:?}"
        );
        assert_eq!(
            gone.error_code.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException, got {gone:?}"
        );
        assert_eq!(
            gone.message(),
            missing_group_message(&group_name),
            "case {case:?}: expected {:?}, got {gone:?}",
            missing_group_message(&group_name)
        );

        // The delete models no not-found, so repeating it removes nothing and succeeds.
        let again = emulator.delete_group(&group_name, None).await;
        assert_eq!(
            again.status, 200,
            "case {case:?}: a repeated delete must succeed, got {again:?}"
        );

        // What was removed is one record and not the registry: the parent is untouched.
        let survivor = emulator.describe_group(parent).await;
        assert_eq!(
            survivor.status, 200,
            "case {case:?}: every other group must survive the delete, got {survivor:?}"
        );
    }

    // A name no group was ever created under is the same success.
    let never = emulator.delete_group("never-created-group", None).await;
    assert_eq!(
        never.status, 200,
        "case \"a group that never existed\": the delete must succeed, got {never:?}"
    );
}

#[tokio::test]
async fn deleting_a_thing_group_with_child_groups_is_refused_until_the_children_are_deleted() {
    let emulator = Emulator::new();

    // The hierarchy is deleted from the leaves up: a group still named as a parent is
    // refused, however many children it has and however deep they go, and it stays readable
    // with its children.
    for count in [1usize, 2] {
        let case = format!("{count} child groups");
        let parent = format!("live-parent-{count}");
        let created = emulator.create_group(&parent, &json!({})).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: creating the parent must succeed, got {created:?}"
        );
        let children: Vec<String> = (0..count).map(|index| format!("{parent}-child-{index}")).collect();
        for child in &children {
            let created = emulator
                .create_group(child, &json!({ "parentGroupName": parent }))
                .await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: creating {child} must succeed, got {created:?}"
            );
        }
        // One grandchild, so the refusal is about the DIRECT children of the group the
        // request named and a deeper level does not change it.
        let grandchild = format!("{parent}-grandchild");
        let created = emulator
            .create_group(&grandchild, &json!({ "parentGroupName": children[0] }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: creating the grandchild must succeed, got {created:?}"
        );

        let refused = emulator.delete_group(&parent, None).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: a live child must refuse the parent's delete 400, got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/"),
            "case {case:?}: expected the namespaced InvalidRequestException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            child_groups_message(&parent),
            "case {case:?}: expected {:?}, got {refused:?}",
            child_groups_message(&parent)
        );
        let read = emulator.describe_group(&parent).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: a refused delete must leave the parent readable, got {read:?}"
        );
        for child in &children {
            let read = emulator.describe_group(child).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: a refused delete must leave {child} readable, got {read:?}"
            );
        }

        // The child holding the grandchild is refused for the same reason, and the leaves
        // delete: the parent only takes its delete once every level below it is gone.
        let refused = emulator.delete_group(&children[0], None).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: the child holding the grandchild must be refused, got {refused:?}"
        );
        let deleted = emulator.delete_group(&grandchild, None).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: the leaf must delete, got {deleted:?}"
        );
        for child in &children {
            let deleted = emulator.delete_group(child, None).await;
            assert_eq!(
                deleted.status, 200,
                "case {case:?}: {child} must delete once it has no children, got {deleted:?}"
            );
        }
        let deleted = emulator.delete_group(&parent, None).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: the parent must delete once its children are gone, got {deleted:?}"
        );
        let gone = emulator.describe_group(&parent).await;
        assert_eq!(
            gone.status, 404,
            "case {case:?}: the deleted parent must be gone, got {gone:?}"
        );
    }
}

#[tokio::test]
async fn deleting_a_dynamic_thing_group_through_the_static_delete_is_refused() {
    let emulator = Emulator::new();

    // A dynamic group is deleted through `DeleteDynamicThingGroup`: naming one here is the
    // modelled 400 and removes nothing, whatever the group's query selects and whether or not
    // the request names the version the record is at. The rows differ in the query the group
    // holds, which is what makes a group dynamic at all.
    for (case, query_string) in [
        ("a query over a thing name", "thingName:sensor*"),
        ("a query over an attribute", "attributes.zone:north"),
    ] {
        let group_name = format!("dynamic-group-{}", case.len());
        let created = emulator
            .create_dynamic_group(&group_name, &json!({ "queryString": query_string }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateDynamicThingGroup must succeed, got {created:?}"
        );

        for expected_version in [None, Some(1i64)] {
            let refused = emulator.delete_group(&group_name, expected_version).await;
            assert_eq!(
                refused.status, 400,
                "case {case:?} with expectedVersion={expected_version:?}: the static delete \
                 must refuse a dynamic group 400, got {refused:?}"
            );
            assert_eq!(
                refused.error_code.as_deref(),
                Some("InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/"),
                "case {case:?}: expected the namespaced InvalidRequestException, got {refused:?}"
            );
            assert_eq!(
                refused.message(),
                dynamic_group_message(&group_name),
                "case {case:?}: expected {:?}, got {refused:?}",
                dynamic_group_message(&group_name)
            );
            let read = emulator.describe_group(&group_name).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: a refused delete must leave the group readable, got {read:?}"
            );
            assert_eq!(
                read.text("queryString").as_deref(),
                Some(query_string),
                "case {case:?}: a refused delete must change nothing, got {read:?}"
            );
        }

        // The operation that OWNS this kind of group deletes it, and then the group is gone.
        let deleted = emulator.delete_dynamic_group(&group_name).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: DeleteDynamicThingGroup must succeed, got {deleted:?}"
        );
        let gone = emulator.describe_group(&group_name).await;
        assert_eq!(
            gone.status, 404,
            "case {case:?}: the deleted dynamic group must be gone, got {gone:?}"
        );
        assert_eq!(
            gone.message(),
            missing_group_message(&group_name),
            "case {case:?}: expected {:?}, got {gone:?}",
            missing_group_message(&group_name)
        );
    }
}

#[tokio::test]
async fn deleting_a_thing_group_at_a_version_the_record_is_not_at_is_refused() {
    let emulator = Emulator::new();

    // `expectedVersion` is "The expected version of the thing group to delete": a request
    // naming a version the record is not at is acting on a group it has not seen, so the
    // delete is the modelled 409 and removes nothing — and the version it has to match is the
    // one the record reached through its updates, which is what `DescribeThingGroup` reports.
    for updates in [0usize, 1, 2] {
        let case = format!("{updates} property updates");
        let group_name = format!("versioned-group-{updates}");
        let created = emulator.create_group(&group_name, &json!({})).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateThingGroup must succeed, got {created:?}"
        );
        for round in 0..updates {
            let description = format!("updated description {round}");
            let updated = emulator
                .update_group(
                    &group_name,
                    &json!({ "thingGroupProperties": { "thingGroupDescription": description } }),
                )
                .await;
            assert_eq!(
                updated.status, 200,
                "case {case:?}: UpdateThingGroup must succeed, got {updated:?}"
            );
        }

        let read = emulator.describe_group(&group_name).await;
        let version = read.version().unwrap_or_else(|| {
            panic!("case {case:?}: the read must report a version, got {read:?}")
        });
        assert_eq!(
            version,
            1 + updates as i64,
            "case {case:?}: a create plus {updates} updates is version {}, got {read:?}",
            1 + updates as i64
        );
        if updates > 0 {
            assert_eq!(
                read.group_description(),
                Some(format!("updated description {}", updates - 1)),
                "case {case:?}: the last update's description must be reported, got {read:?}"
            );
        }

        for wrong in [version - 1, version + 1] {
            let refused = emulator.delete_group(&group_name, Some(wrong)).await;
            assert_eq!(
                refused.status, 409,
                "case {case:?}: expectedVersion {wrong} must be refused 409, got {refused:?}"
            );
            assert_eq!(
                refused.error_code.as_deref(),
                Some("VersionConflictException"),
                "case {case:?}: expected VersionConflictException, got {refused:?}"
            );
            assert_eq!(
                refused.message(),
                group_version_conflict_message(&group_name),
                "case {case:?}: expected {:?}, got {refused:?}",
                group_version_conflict_message(&group_name)
            );
            let still = emulator.describe_group(&group_name).await;
            assert_eq!(
                still.status, 200,
                "case {case:?}: a refused delete must leave the group readable, got {still:?}"
            );
            assert_eq!(
                still.version(),
                Some(version),
                "case {case:?}: a refused delete must not move the version, got {still:?}"
            );
        }

        let deleted = emulator.delete_group(&group_name, Some(version)).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: the matching expectedVersion must delete, got {deleted:?}"
        );
        let gone = emulator.describe_group(&group_name).await;
        assert_eq!(
            gone.status, 404,
            "case {case:?}: the deleted group must be gone, got {gone:?}"
        );
    }
}
