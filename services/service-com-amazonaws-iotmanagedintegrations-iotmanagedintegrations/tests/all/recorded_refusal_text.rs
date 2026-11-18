// SPDX-License-Identifier: CC0-1.0

//! What a refusal of this service SAYS, over the real restJson1 request path
//! (`MultiServiceRouter::dispatch`).
//!
//! RULE: a refusal's `Message` is the sentence the service itself words for that refusal
//! class, and nothing of the request is interpolated into it unless the sentence's own
//! grammar has a slot for it. Three parts of that rule are asserted here, each over a table
//! that spans the class rather than one recorded request:
//!
//! 1. a `@pattern` clause quotes the expression the service PUBLISHES for the member's
//!    shape. Smithy writes `^`/`$` around that expression to say "the whole value matches",
//!    so the pair is the model's wrapping: the clause quotes what is inside it, which is
//!    what a client that lifts the expression out of the message and re-validates locally
//!    compiles. The CHECK is unaffected — the modelled trait is still applied as written, so
//!    exactly the same values are accepted.
//! 2. a refusal whose sentence is FIXED states that sentence verbatim and quotes no
//!    identifier, member name, enum value or ARN from the request — including the
//!    operation-specific ones, which differ between two operations bound to the same URI.
//! 3. a create refused because the resource it would mint is already there states its OWN
//!    resource's fixed sentence and interpolates nothing — neither the target or level the
//!    request named nor the identifier or name of the record already stored.
//!
//! Each row names the modelled shape its expression or its sentence belongs to, and the
//! rows differ along what the rule generalises over: the operation, the member and its
//! binding (body, URI label), whether the member is `@sensitive`, the shape of the
//! expression (ASCII punctuation and backslashes, Unicode property classes, a `*`
//! quantifier, an alternation whose branches anchor themselves), the identifier's form and
//! length, the region the request is signed for, and whether the store already holds a
//! record.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::state::{
    HUB_NETWORK_MODE_VALUES, LOG_LEVEL_VALUES,
};
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test's requests are signed for unless a case names another.
const REGION: &str = "us-east-1";

/// The wire error codes the model's `@httpError` shapes are rendered under.
const VALIDATION: &str = "ValidationException";
const NOT_FOUND: &str = "ResourceNotFoundException";
const CONFLICT: &str = "ConflictException";

/// The account id every generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`), which
/// the ARNs the rows below carry are built for.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The sentence `PutDefaultEncryptionConfiguration` refuses a request for customer-key
/// encryption that names no key with, as this service words it: the whole string is fixed
/// text and no request value appears in it.
const KEY_NOT_AUTHORIZED: &str = "IoT Managed Integrations is not authorized to use the key.";

/// The sentence `PutRuntimeLogConfiguration` states for a `ManagedThingId` no managed thing
/// of the region is held under — phrased around the runtime log configuration the call was
/// installing, and interpolating no value of the request.
const PUT_MANAGED_THING_ABSENT: &str =
    "The runtime log configuration is for a managed thing resource that cannot be found.";

/// The sentence `GetRuntimeLogConfiguration` states for a managed thing holding no
/// configuration: this operation's own, which differs from the put's above although the two
/// are bound to the same URI.
const GET_CONFIGURATION_ABSENT: &str =
    "Runtime log configuration for the requested managedThingId doesn't exist";

/// The sentence a put against a managed thing that runs no managed-integrations runtime is
/// refused with: the device classes runtime logging is supported for, and no request value.
const RUNTIME_LOGGING_UNSUPPORTED: &str = "Only 1P device or hub is supported for Runtime logging";

/// The sentence a `CreateEventLogConfiguration` is refused with when the account already
/// holds an event log configuration for the log target the request names, as the log backend
/// words it: the whole string is fixed text.
const LOG_CONFIGURATION_EXISTS: &str = "Log configuration already exists.";

/// The sentence a `CreateDestination` is refused with when the region already holds a
/// destination under the name the request names — the sibling refusal of the same class,
/// worded by its own backend, which is why neither sentence is derived from the other.
const DESTINATION_EXISTS: &str = "Destination already exists";

/// One HTTP answer, reduced to what the assertions read.
struct Reply {
    status: u16,
    /// The restJson1 error code, from `x-amzn-errortype`.
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    fn member(&self, name: &str) -> Option<&Value> {
        self.body.get(name).filter(|value| !value.is_null())
    }

    fn string(&self, name: &str) -> Option<&str> {
        self.member(name).and_then(Value::as_str)
    }

    /// The reason an error body carries, on the member the model declares for it.
    fn message(&self) -> &str {
        self.string("Message")
            .or_else(|| self.string("message"))
            .unwrap_or_default()
    }
}

/// A router holding one `iotmanagedintegrations` service over a fresh shared engine: the
/// same assembly `core/router` performs, with one service mounted.
fn emulator() -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    // The signature itself is not under test here, so requests are unauthenticated.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::new(engine::InMemoryEngine::new())));
    router
}

/// Send one request, signed for `region`, and reduce the answer. The `Authorization`
/// credential scope is what routes the request to this service and names its region.
async fn call_in(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Reply {
    let payload = body.map_or_else(bytes::Bytes::new, |value| {
        bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize the request body"))
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://iotmanagedintegrations.{region}.amazonaws.com{path}"
        ))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTESTTEST/20260101/{region}/\
                 iotmanagedintegrations/aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
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
        error_type,
        body,
    }
}

/// [`call_in`] in [`REGION`].
async fn call(router: &MultiServiceRouter, method: &str, path: &str, body: Option<Value>) -> Reply {
    call_in(router, REGION, method, path, body).await
}

/// The AWS validation envelope around ONE clause, as this service states it: the violation
/// count, the member path in lowerCamel, and the requirement — with the rejected value left
/// out of the clause's opening.
fn envelope(member_path: &str, requirement: &str) -> String {
    format!("1 validation error detected: Value at '{member_path}' failed to satisfy constraint: {requirement}")
}

/// The requirement a `@pattern` violation states, quoting `expression`.
fn pattern_requirement(expression: &str) -> String {
    format!("Member must satisfy regular expression pattern: {expression}")
}

/// One row of the `@pattern` table.
struct PatternCase {
    label: &'static str,
    method: &'static str,
    path: String,
    /// The request that violates the member's `@pattern` and satisfies everything else, so
    /// the envelope counts exactly one error.
    body: Option<Value>,
    /// The member path the clause names, in lowerCamel as the wire names it.
    member_path: &'static str,
    /// The `@pattern` trait text of the member's shape, exactly as the model declares it.
    modelled: &'static str,
    /// The expression the clause quotes: the modelled text inside its whole-value anchors,
    /// or the modelled text itself where its own BRANCHES carry the anchors at its ends.
    published: &'static str,
    /// Whether the modelled text anchors its own branches, so the `^`/`$` at its ends
    /// belong to those branches and the expression is published as the model declares it.
    inner_anchors: bool,
    /// A value the member's own `@pattern` admits, used as the positive control.
    legal: Option<Value>,
}

/// A `DeliveryDestinationArn` of `REGION`, inside the modelled `@pattern`
/// (`smithy_query(query="operation",operation="CreateDestination")` -> `DeliveryDestinationArn`
/// pattern `^arn:aws:[0-9a-zA-Z]+:[0-9a-zA-Z-]+:[0-9]+:[0-9a-zA-Z]+/[0-9a-zA-Z._-]+$`).
fn delivery_destination_arn() -> String {
    format!("arn:aws:kinesis:{REGION}:{ACCOUNT}:stream/managed-integrations-events")
}

/// A `RoleArn` of this account, for the same create.
fn delivery_role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/service-role/managed-integrations-delivery")
}

/// RULE: a `@pattern` clause quotes the expression the service publishes for the member's
/// shape — the modelled trait text WITHOUT the whole-value anchors — for every operation,
/// every member and every binding; and a value the modelled expression admits is not refused
/// for its pattern at all, so the rendering changed no decision.
#[tokio::test]
async fn a_pattern_clause_quotes_the_published_expression() {
    let router = emulator();

    // Every row's modelled text is the member's own `@pattern`, from
    // `smithy_query(query="shape",shape="<the member's target>")`.
    let cases: Vec<PatternCase> = vec![
        // `AuthMaterialString` (`CreateManagedThing.AuthenticationMaterial`): the recorded
        // row. The member is `@sensitive`, so the clause names the path and the expression
        // and publishes nothing of the value; the expression carries ASCII punctuation,
        // escaped metacharacters and a backslash, none of which the rendering may disturb.
        PatternCase {
            label: "CreateManagedThing/authenticationMaterial (a @sensitive member)",
            method: "POST",
            path: "/managed-things".to_string(),
            body: Some(json!({
                "Role": "CONTROLLER",
                "AuthenticationMaterialType": "WIFI_SETUP_QR_BAR_CODE",
                // A non-ASCII letter: outside the character class the expression admits.
                "AuthenticationMaterial": "WIFI:S:setup;P:pässword;;",
            })),
            member_path: "authenticationMaterial",
            modelled: r#"^[0-9A-Za-z!#$%&()*\+\-;<=>?@^_`{|}~\/: {},\\"]+$"#,
            published: r#"[0-9A-Za-z!#$%&()*\+\-;<=>?@^_`{|}~\/: {},\\"]+"#,
            inner_anchors: false,
            legal: Some(json!({
                "Role": "CONTROLLER",
                "AuthenticationMaterialType": "WIFI_SETUP_QR_BAR_CODE",
                "AuthenticationMaterial": "WIFI:S:setup;P:password;;",
            })),
        },
        // `DestinationName` (`CreateDestination.Name`): an expression whose classes are
        // UNICODE PROPERTIES rather than ASCII ranges — a member the report that prompted
        // this rule did not cite.
        PatternCase {
            label: "CreateDestination/name (Unicode property classes)",
            method: "POST",
            path: "/destinations".to_string(),
            body: Some(json!({
                "Name": "events*stream",
                "DeliveryDestinationArn": delivery_destination_arn(),
                "DeliveryDestinationType": "KINESIS",
                "RoleArn": delivery_role_arn(),
            })),
            member_path: "name",
            modelled: r"^[\p{L}\p{N} ._-]+$",
            published: r"[\p{L}\p{N} ._-]+",
            inner_anchors: false,
            legal: Some(json!({
                "Name": "events.stream-1",
                "DeliveryDestinationArn": delivery_destination_arn(),
                "DeliveryDestinationType": "KINESIS",
                "RoleArn": delivery_role_arn(),
            })),
        },
        // `CredentialLockerName` (`CreateCredentialLocker.Name`): a `@sensitive` member of
        // another operation, whose expression admits a space and two punctuation marks.
        PatternCase {
            label: "CreateCredentialLocker/name",
            method: "POST",
            path: "/credential-lockers".to_string(),
            body: Some(json!({"Name": "locker/one"})),
            member_path: "name",
            modelled: r"^[A-Za-z0-9-_ ]+$",
            published: r"[A-Za-z0-9-_ ]+",
            inner_anchors: false,
            legal: Some(json!({"Name": "locker one"})),
        },
        // `ProvisioningProfileName` (`CreateProvisioningProfile.Name`): an operation that is
        // still a scaffold, so the clause does not wait for a handler to exist. No positive
        // control: the accepted request would reach that scaffold's 501.
        PatternCase {
            label: "CreateProvisioningProfile/name (a scaffolded operation)",
            method: "POST",
            path: "/provisioning-profiles".to_string(),
            body: Some(json!({"ProvisioningType": "JITR", "Name": "not a legal name"})),
            member_path: "name",
            modelled: r"^[0-9A-Za-z_-]+$",
            published: r"[0-9A-Za-z_-]+",
            inner_anchors: false,
            legal: None,
        },
        // `ManagedThingId` (`GetManagedThing.Identifier`): a member bound to the URI LABEL
        // rather than to the body, on a read, whose expression quantifies with `*`.
        PatternCase {
            label: "GetManagedThing/identifier (a URI label)",
            method: "GET",
            path: "/managed-things/thing.with.dots".to_string(),
            body: None,
            member_path: "identifier",
            modelled: r"^[a-zA-Z0-9:_-]*$",
            published: r"[a-zA-Z0-9:_-]*",
            inner_anchors: false,
            legal: None,
        },
        // `SmartHomeResourceType` (`CreateEventLogConfiguration.ResourceType`): an
        // expression whose own BRANCHES carry the anchors at its ends, so it is published as
        // the model declares it — dropping those two would leave an alternative that is a
        // bare end-of-input anchor and drop the branch it belonged to.
        PatternCase {
            label: "CreateEventLogConfiguration/resourceType (an expression that anchors \
                    its own branches)",
            method: "POST",
            path: "/event-log-configurations".to_string(),
            body: Some(json!({
                "ResourceType": "device/light",
                "EventLogLevel": "INFO",
            })),
            member_path: "resourceType",
            modelled: r"^[*]$|^(managed-thing|credential-locker|provisioning-profile|ota-task|account-association)$",
            published: r"^[*]$|^(managed-thing|credential-locker|provisioning-profile|ota-task|account-association)$",
            inner_anchors: true,
            legal: Some(json!({
                "ResourceType": "managed-thing",
                "EventLogLevel": "INFO",
            })),
        },
    ];

    for case in &cases {
        let label = case.label;

        // The row's own statement of the rule: an expression the model WRAPPED is published
        // inside that pair and carries no anchor itself, and one whose branches carry the
        // anchors at its ends is published exactly as the model declares it.
        if case.inner_anchors {
            assert_eq!(
                case.published, case.modelled,
                "case {label:?}: an expression that anchors its own branches is published as \
                 declared"
            );
        } else {
            assert_eq!(
                case.modelled,
                format!("^{}$", case.published),
                "case {label:?}: the row's published expression must be the modelled trait \
                 text inside its anchors, got {:?} against {:?}",
                case.published,
                case.modelled
            );
            assert!(
                !case.published.starts_with('^') && !case.published.ends_with('$'),
                "case {label:?}: the published expression carries no anchor, got {:?}",
                case.published
            );
        }

        let reply = call(&router, case.method, &case.path, case.body.clone()).await;
        assert_eq!(
            reply.status, 400,
            "case {label:?}: expected HTTP 400, got {} with body {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_type.as_deref(),
            Some(VALIDATION),
            "case {label:?}: expected the modelled {VALIDATION} code, got {:?}",
            reply.error_type
        );
        assert_eq!(
            reply.message(),
            envelope(case.member_path, &pattern_requirement(case.published)),
            "case {label:?}: expected the clause to quote the published expression, got {:?}",
            reply.message()
        );
        assert!(
            case.inner_anchors || !reply.message().contains(case.modelled),
            "case {label:?}: the clause must not quote the modelled text with its anchors, \
             got {:?}",
            reply.message()
        );

        // The positive control: a value the SAME modelled expression admits is not refused
        // for its pattern, so only the rendering of the requirement is this service's.
        if let Some(legal) = &case.legal {
            let accepted = call(&router, case.method, &case.path, Some(legal.clone())).await;
            assert!(
                !accepted.message().contains("regular expression pattern"),
                "case {label:?}: a value the expression admits is not refused for its \
                 pattern, got {} with body {:?}",
                accepted.status,
                accepted.body
            );
        }
    }
}

/// RULE: a requirement of another constraint class is untouched by the pattern rendering — a
/// `@length` clause states its bound and quotes no expression at all.
#[tokio::test]
async fn a_requirement_of_another_class_quotes_no_expression() {
    let router = emulator();

    // `ProvisioningProfileName`: `@length` 1..36, so one character past the maximum violates
    // the length and nothing else.
    let too_long = "a".repeat(37);
    let reply = call(
        &router,
        "POST",
        "/provisioning-profiles",
        Some(json!({"ProvisioningType": "FLEET_PROVISIONING", "Name": too_long})),
    )
    .await;
    assert_eq!(
        reply.message(),
        envelope("name", "Member must have length less than or equal to 36"),
        "case \"past the length maximum\": expected the length requirement, got {:?}",
        reply.message()
    );
}

/// RULE: a refusal whose sentence is FIXED states exactly that sentence and interpolates no
/// value of the request — whatever region the call was signed for, whatever the store already
/// holds, and whatever form the identifier took.
#[tokio::test]
async fn a_fixed_refusal_sentence_is_stated_verbatim() {
    // The rows differ along the region, and along whether a configuration is already in
    // force in it: neither changes the sentence.
    for region in ["us-east-1", "eu-central-1", "ap-southeast-2"] {
        for configured in [false, true] {
            let router = emulator();
            let arn = format!("arn:aws:kms:{region}:{ACCOUNT}:key/1234abcd-12ab-34cd-56ef-1234567890ab");
            if configured {
                let seeded = call_in(
                    &router,
                    region,
                    "POST",
                    "/configuration/account/encryption",
                    Some(json!({
                        "encryptionType": "CUSTOMER_KEY_ENCRYPTION",
                        "kmsKeyArn": arn.clone(),
                    })),
                )
                .await;
                assert_eq!(
                    seeded.status, 201,
                    "case {region:?}: expected HTTP 201 seeding the configuration, got {} with \
                     body {:?}",
                    seeded.status, seeded.body
                );
            }

            let case = format!("{region}/{}", if configured { "configured" } else { "default" });
            let refused = call_in(
                &router,
                region,
                "POST",
                "/configuration/account/encryption",
                Some(json!({"encryptionType": "CUSTOMER_KEY_ENCRYPTION"})),
            )
            .await;
            assert_eq!(
                refused.status, 400,
                "case {case:?}: expected HTTP 400, got {} with body {:?}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.error_type.as_deref(),
                Some(VALIDATION),
                "case {case:?}: expected the modelled {VALIDATION} code, got {:?}",
                refused.error_type
            );
            assert_eq!(
                refused.message(),
                KEY_NOT_AUTHORIZED,
                "case {case:?}: expected the service's own sentence, got {:?}",
                refused.message()
            );
            for quoted in ["kmsKeyArn", "CUSTOMER_KEY_ENCRYPTION", arn.as_str()] {
                assert!(
                    !refused.message().contains(quoted),
                    "case {case:?}: the sentence interpolates nothing of the request, yet it \
                     carries {quoted:?}: {:?}",
                    refused.message()
                );
            }
        }
    }
}

/// Create a managed thing in `region` and answer its generated id.
async fn managed_thing_in(router: &MultiServiceRouter, region: &str, case: &str) -> String {
    let created = call_in(
        router,
        region,
        "POST",
        "/managed-things",
        Some(json!({
            "Role": "CONTROLLER",
            "AuthenticationMaterialType": "WIFI_SETUP_QR_BAR_CODE",
            "AuthenticationMaterial": "WIFI:S:setup;P:password;;",
        })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: expected HTTP 201 from CreateManagedThing, got {} with body {:?}",
        created.status, created.body
    );
    created
        .string("Id")
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected an Id, got body {:?}", created.body)
        })
        .to_string()
}

/// RULE: the two operations bound to the runtime-log-configuration URI word their OWN
/// absence sentences — the put's around the configuration's missing managed thing, the get's
/// around the missing configuration — and neither quotes the `ManagedThingId` the request
/// carried.
#[tokio::test]
async fn each_runtime_log_operation_states_its_own_absence_sentence() {
    let router = emulator();

    // A managed thing another region's endpoint created, to address from this one.
    let elsewhere = managed_thing_in(&router, "eu-central-1", "eu-central-1 thing").await;

    // The rows differ along the FORM and length of a `ManagedThingId` this region holds no
    // managed thing under, inside the member's `@pattern` (`^[a-zA-Z0-9:_-]*$`) and
    // `@length` (1..64): nothing ever created it, or another region did.
    let absent: Vec<(String, String)> = vec![
        ("never created".to_string(), "sim2realbogusmanagedthingid".to_string()),
        ("mixed case".to_string(), "NoSuchManagedThing1".to_string()),
        ("the punctuation the pattern admits".to_string(), "thing:1_2-3".to_string()),
        ("one character".to_string(), "9".to_string()),
        ("the modelled length maximum".to_string(), "b".repeat(64)),
        ("another region's managed thing".to_string(), elsewhere),
    ];

    let configuration = json!({"RuntimeLogConfigurations": {"LogLevel": "INFO"}});
    for (why, id) in &absent {
        let path = format!("/runtime-log-configurations/{id}");
        let refused = call(&router, "PUT", &path, Some(configuration.clone())).await;
        assert_eq!(
            refused.status, 404,
            "case {why:?}: expected HTTP 404, got {} with body {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some(NOT_FOUND),
            "case {why:?}: expected the modelled {NOT_FOUND} code, got {:?}",
            refused.error_type
        );
        assert_eq!(
            refused.message(),
            PUT_MANAGED_THING_ABSENT,
            "case {why:?}: expected the put's own sentence, got {:?}",
            refused.message()
        );
        assert!(
            !refused.message().contains(id.as_str()),
            "case {why:?}: the sentence must not carry the submitted identifier, got {:?}",
            refused.message()
        );
    }

    // A managed thing this region DOES hold, with nothing installed on it: the get states
    // the configuration's absence in its own words, which are not the put's.
    for (case, mode) in HUB_NETWORK_MODE_VALUES.iter().enumerate() {
        let case = format!("a {mode} hub ({case})");
        let id = managed_thing_in(&router, REGION, &case).await;
        let updated = call(
            &router,
            "PUT",
            &format!("/managed-things/{id}"),
            Some(json!({"HubNetworkMode": mode})),
        )
        .await;
        assert_eq!(
            updated.status, 204,
            "case {case:?}: expected HTTP 204 from UpdateManagedThing, got {} with body {:?}",
            updated.status, updated.body
        );

        let path = format!("/runtime-log-configurations/{id}");
        let read = call(&router, "GET", &path, None).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: expected HTTP 404, got {} with body {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.message(),
            GET_CONFIGURATION_ABSENT,
            "case {case:?}: expected the get's own sentence, got {:?}",
            read.message()
        );
        assert!(
            !read.message().contains(id.as_str()),
            "case {case:?}: the sentence must not carry the submitted identifier, got {:?}",
            read.message()
        );
        assert_ne!(
            read.message(),
            PUT_MANAGED_THING_ABSENT,
            "case {case:?}: the read states its own absence, not the put's"
        );
    }

    // And the refusal of a put against a managed thing that runs no runtime of this service
    // states which device classes runtime logging is for, quoting nothing of the request.
    let third_party = managed_thing_in(&router, REGION, "a third-party device").await;
    let unsupported = call(
        &router,
        "PUT",
        &format!("/runtime-log-configurations/{third_party}"),
        Some(configuration),
    )
    .await;
    assert_eq!(
        unsupported.status, 400,
        "case \"a third-party device\": expected HTTP 400, got {} with body {:?}",
        unsupported.status, unsupported.body
    );
    assert_eq!(
        unsupported.message(),
        RUNTIME_LOGGING_UNSUPPORTED,
        "case \"a third-party device\": expected the service's own sentence, got {:?}",
        unsupported.message()
    );
    assert!(
        !unsupported.message().contains(third_party.as_str()),
        "case \"a third-party device\": the sentence must not carry the submitted identifier, \
         got {:?}",
        unsupported.message()
    );
}

/// The body of a `CreateEventLogConfiguration` for one log target at one level, carrying
/// `ResourceId` only when the target names a resource.
fn event_log_request(resource_type: &str, resource_id: Option<&str>, level: &str) -> Value {
    let mut body = json!({"ResourceType": resource_type, "EventLogLevel": level});
    if let Some(resource_id) = resource_id {
        body["ResourceId"] = json!(resource_id);
    }
    body
}

/// One row of the duplicate-create table: the log target a create names, and the level the
/// first of those creates stored against it.
struct DuplicateTarget {
    label: &'static str,
    /// The region the pair of creates is signed for: the record and the conflict are both
    /// regional, and the sentence is the same in every one of them.
    region: &'static str,
    resource_type: &'static str,
    resource_id: Option<String>,
    first_level: &'static str,
}

/// The `@length` maximum of `SmartHomeResourceId`, in characters
/// (`smithy_query(query="shape",shape="SmartHomeResourceId")` -> length 1..200).
const RESOURCE_ID_MAX_LENGTH: usize = 200;

/// RULE: a create refused because the resource it would mint is already there answers the
/// modelled `ConflictException` (HTTP 409) carrying the FIXED sentence its own resource's
/// backend words, interpolating nothing — not the target or level the request named, and not
/// the identifier or name of the record already stored.
#[tokio::test]
async fn a_duplicate_create_states_its_resources_fixed_sentence() {
    // The rows differ along what the rule generalises over: the resource type the target
    // names (the account wildcard, and types the `SmartHomeResourceType` `@pattern` admits),
    // whether the target names a resource inside that type, the form and length of the
    // `ResourceId` inside its `@pattern` (`^[a-zA-Z0-9+*]*$`) and `@length` (1..200), the
    // level stored first, and the region the calls are signed for.
    let rows: [DuplicateTarget; 6] = [
        DuplicateTarget {
            label: "the account wildcard",
            region: REGION,
            resource_type: "*",
            resource_id: None,
            first_level: "INFO",
        },
        DuplicateTarget {
            label: "a whole resource type",
            region: REGION,
            resource_type: "ota-task",
            resource_id: None,
            first_level: "ERROR",
        },
        DuplicateTarget {
            label: "a resource inside a type",
            region: REGION,
            resource_type: "managed-thing",
            resource_id: Some("sim2realthingone".to_string()),
            first_level: "DEBUG",
        },
        DuplicateTarget {
            label: "a mixed-case resource id with the punctuation the pattern admits",
            region: "eu-central-1",
            resource_type: "account-association",
            resource_id: Some("Assoc9+Wildcard*".to_string()),
            first_level: "WARN",
        },
        DuplicateTarget {
            label: "another region, another type",
            region: "ap-southeast-2",
            resource_type: "credential-locker",
            resource_id: Some("lockerninetynine".to_string()),
            first_level: "INFO",
        },
        DuplicateTarget {
            label: "a resource id at the modelled length maximum",
            region: "eu-west-1",
            resource_type: "provisioning-profile",
            resource_id: Some("a".repeat(RESOURCE_ID_MAX_LENGTH)),
            first_level: "DEBUG",
        },
    ];

    for row in &rows {
        // A fresh store per row, so the only configuration it holds is the row's own.
        let router = emulator();
        let created = call_in(
            &router,
            row.region,
            "POST",
            "/event-log-configurations",
            Some(event_log_request(
                row.resource_type,
                row.resource_id.as_deref(),
                row.first_level,
            )),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {:?}: expected HTTP 201 from the first create, got {} with body {:?}",
            row.label, created.status, created.body
        );
        let id = created
            .string("Id")
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected an Id, got body {:?}",
                    row.label, created.body
                )
            })
            .to_string();

        // The duplicate is refused whatever level it asks for — the whole `LogLevel` value
        // set — and the sentence it carries never changes.
        for level in LOG_LEVEL_VALUES {
            let case = format!("{}/again at {level}", row.label);
            let refused = call_in(
                &router,
                row.region,
                "POST",
                "/event-log-configurations",
                Some(event_log_request(
                    row.resource_type,
                    row.resource_id.as_deref(),
                    level,
                )),
            )
            .await;
            assert_eq!(
                refused.status, 409,
                "case {case:?}: expected HTTP 409, got {} with body {:?}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.error_type.as_deref(),
                Some(CONFLICT),
                "case {case:?}: expected the modelled {CONFLICT} code, got {:?}",
                refused.error_type
            );
            assert_eq!(
                refused.message(),
                LOG_CONFIGURATION_EXISTS,
                "case {case:?}: expected the log backend's fixed sentence, got {:?}",
                refused.message()
            );

            // Nothing of the request, and nothing of the record already stored, appears in
            // it. A value of one or two characters is left out of this check: such a value
            // is a letter of the sentence itself and says nothing about interpolation.
            let mut quoted: Vec<&str> = vec![id.as_str(), row.resource_type, level, row.first_level];
            quoted.extend(row.resource_id.as_deref());
            for value in quoted.into_iter().filter(|value| value.len() > 2) {
                assert!(
                    !refused.message().contains(value),
                    "case {case:?}: the sentence interpolates nothing, yet it carries \
                     {value:?}: {:?}",
                    refused.message()
                );
            }
        }
    }

    // The same rule in another resource of this service, whose backend words the refusal its
    // own way: the two sentences are each their resource's, so neither is derived from the
    // other. The rows differ along the form of the name repeated.
    for name in ["events.stream-1", "Prod Stream", "destination_99"] {
        let body = json!({
            "Name": name,
            "DeliveryDestinationArn": delivery_destination_arn(),
            "DeliveryDestinationType": "KINESIS",
            "RoleArn": delivery_role_arn(),
        });
        let router = emulator();
        let created = call(&router, "POST", "/destinations", Some(body.clone())).await;
        assert_eq!(
            created.status, 201,
            "case {name:?}: expected HTTP 201 from the first create, got {} with body {:?}",
            created.status, created.body
        );

        let refused = call(&router, "POST", "/destinations", Some(body)).await;
        assert_eq!(
            refused.status, 409,
            "case {name:?}: expected HTTP 409, got {} with body {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some(CONFLICT),
            "case {name:?}: expected the modelled {CONFLICT} code, got {:?}",
            refused.error_type
        );
        assert_eq!(
            refused.message(),
            DESTINATION_EXISTS,
            "case {name:?}: expected the destination backend's own sentence, got {:?}",
            refused.message()
        );
        assert!(
            !refused.message().contains(name),
            "case {name:?}: the sentence quotes no name, got {:?}",
            refused.message()
        );
    }
}
