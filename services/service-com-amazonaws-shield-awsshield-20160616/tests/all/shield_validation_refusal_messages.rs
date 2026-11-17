// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the TEXT this service refuses a request with.
//!
//! Every case goes over the wire — an `awsJson1_1` `POST /` with
//! `X-Amz-Target: AWSShield_20160616.<Operation>` through the same
//! `MultiServiceRouter` the production router serves — so routing, deserialization,
//! modelled-constraint validation, the handler and serialization are all exercised.
//! A client separates this service's same-code failures by their message, so the
//! message is part of the contract, and no value the caller sent may appear in it.
//!
//! Four rules are under test:
//!
//! * a request that breaks one of the model's own rules — an absent required member, a
//!   value outside a declared length or range, a string outside its pattern, a value
//!   outside an enum — is refused with HTTP 400 `ValidationException` and ONE sentence:
//!   `Error validating the following inputs: ` followed by the refused inputs, named in
//!   lowerCamel and comma-separated, with no submitted value and no constraint from the
//!   model in it;
//! * an `InvalidParameterException` this service raises because a particular input
//!   member is at fault says so in the modelled members it has for the purpose: `reason`
//!   is `FIELD_VALIDATION_FAILED` and `fields` carries one entry per member at fault,
//!   naming it and the fixed sentence saying why it failed, so a client can localize the
//!   fault without reading the top-level sentence;
//! * a `DescribeProtection` that names both of its addresses, or neither, is refused
//!   with `InvalidParameterException` and the fixed sentence stating the requirement,
//!   which carries nothing from the request;
//! * a protection addressed on an account with no live subscription reports the
//!   account-level absence, and one that is simply not there reports the resource class
//!   rather than the identifier the call carried.
//!
//! The tables span what each rule generalises over: the constraint kind, the operation,
//! the member (top-level and nested), the number of refused inputs, each published
//! bound of a range and one past it, and both of the addresses a protection has.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_shield_awsshield_20160616::ops::types::PROTECTED_RESOURCE_TYPE_VALUES;
use service_com_amazonaws_shield_awsshield_20160616::state::service_state;
use service_com_amazonaws_shield_awsshield_20160616::EmulatedService;

/// The `X-Amz-Target` prefix from this crate's `OPERATIONS.md` wire-protocol section.
const TARGET_PREFIX: &str = "AWSShield_20160616.";

/// The fixed text of this service's model-rule refusal, spelled out here rather than
/// read from the service's own constant so the wording itself is pinned.
const VALIDATION_MESSAGE_PREFIX: &str = "Error validating the following inputs: ";

/// How this service states that a call named both of `DescribeProtection`'s addresses.
const BOTH_ADDRESSES_MESSAGE: &str =
    "Invalid parameter. You must provide one value, either protectionId or resourceArn, \
     but not both.";

/// How this service states that a call named neither of them.
const NO_ADDRESS_MESSAGE: &str =
    "Invalid parameter. You must provide one value, either protectionId or resourceArn.";

/// How this service states that the account holds no Shield Advanced subscription.
const SUBSCRIPTION_NOT_FOUND_MESSAGE: &str = "The subscription does not exist.";

/// How this service states that the protection a call addresses is not there.
const PROTECTION_NOT_FOUND_MESSAGE: &str = "The referenced protection does not exist.";

/// A protectable resource ARN, in one of the layouts `Protection$ResourceArn` documents
/// (`arn:aws:ec2:<region>:<account-id>:eip-allocation/<allocation-id>`).
const PROTECTED_RESOURCE_ARN: &str =
    "arn:aws:ec2:us-east-1:000000000000:eip-allocation/eipalloc-0123456789abcdef0";

/// A second protectable resource ARN, in the CloudFront layout the same member documents.
const OTHER_RESOURCE_ARN: &str =
    "arn:aws:cloudfront::000000000000:distribution/E1XMPLDISTRIBUTION";

/// A well-formed `ProtectionId` that no account holds: the model puts `len 36..36` and
/// `^[a-zA-Z0-9\-]*$` on `ProtectionId`, so this passes validation and reaches the
/// lookup.
const UNHELD_PROTECTION_ID: &str = "00000000-0000-4000-8000-000000000000";

/// One answer from the emulator: the HTTP status and the parsed JSON body.
#[derive(Debug)]
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    /// The wire error code (`__type`), or `""` for a success. A `__type` may be
    /// namespaced (`com.amazonaws.shield#Foo`); the code clients branch on is the shape
    /// name.
    fn error_code(&self) -> &str {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .map(|t| t.rsplit('#').next().unwrap_or(t))
            .unwrap_or_default()
    }

    /// The error message as the envelope carries it, under either spelling the
    /// `awsJson1_1` error shape may use.
    fn error_message(&self) -> &str {
        self.body
            .get("message")
            .or_else(|| self.body.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The `reason` the exception classifies itself with, or `""` when the member is
    /// absent from the body.
    fn error_reason(&self) -> &str {
        self.body
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The `fields` entries as `(name, message)` pairs in body order, or `None` when the
    /// member is absent from the body.
    fn error_fields(&self) -> Option<Vec<(String, String)>> {
        let entry = |field: &Value| {
            let member = |name: &str| {
                field
                    .get(name)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            (member("name"), member("message"))
        };
        Some(self.body.get("fields")?.as_array()?.iter().map(entry).collect())
    }
}

/// The emulator under test: one shared engine, one registered service, requests
/// dispatched through the production router.
struct Emulator {
    router: smithy_server_runtime::MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = smithy_server_runtime::MultiServiceRouter::new();
        // Signature verification is the emulator's concern, not this service's; the
        // routing key still comes from the `Authorization` header below.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    async fn call(&self, operation: &str, body: &Value) -> Reply {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("x-amz-target", format!("{TARGET_PREFIX}{operation}"))
            .header("content-type", "application/x-amz-json-1.1")
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/us-east-1/shield/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0123456789abcdef",
            )
            .body(bytes::Bytes::from(body.to_string()))
            .expect("a well-formed test request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                panic!(
                    "{operation}: response body is not JSON ({e}): {}",
                    String::from_utf8_lossy(&bytes)
                )
            })
        };
        Reply { status, body }
    }

    /// Activate Shield Advanced, asserting the activation succeeded.
    async fn subscribe(&self) {
        let reply = self.call("CreateSubscription", &json!({})).await;
        assert_eq!(
            reply.status, 200,
            "case CreateSubscription: expected HTTP 200, got {reply:?}"
        );
    }

    /// Protect `resource_arn`, returning the minted `ProtectionId`.
    async fn create_protection(&self, name: &str, resource_arn: &str) -> String {
        let reply = self
            .call(
                "CreateProtection",
                &json!({ "Name": name, "ResourceArn": resource_arn }),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case CreateProtection {name}: expected HTTP 200, got {reply:?}"
        );
        reply
            .body
            .get("ProtectionId")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case CreateProtection {name}: no ProtectionId in {reply:?}"))
            .to_string()
    }

    /// Move the stored subscription's end to `end_time`, the way the passage of time
    /// would. Reaches the store directly because no modelled operation sets the end of a
    /// subscription.
    fn set_subscription_end(&self, end_time: SystemTime) {
        let state = service_state(&*self.engine).expect("the shield store");
        let mut guard = state.lock();
        let subscription = guard
            .subscription
            .as_mut()
            .expect("an activated subscription");
        subscription.end_time = end_time;
    }

    /// Mark the stored subscription's proactive engagement enabled. Reaches the store
    /// directly because the operations that turn it on
    /// (`EnableProactiveEngagement`, `AssociateProactiveEngagementDetails`) are not
    /// implemented yet.
    fn enable_proactive_engagement(&self) {
        let state = service_state(&*self.engine).expect("the shield store");
        let mut guard = state.lock();
        let subscription = guard
            .subscription
            .as_mut()
            .expect("an activated subscription");
        subscription.proactive_engagement_status = "ENABLED".to_string();
    }

    /// The `ProtectionArn` of a protection, read back through `DescribeProtection`.
    async fn protection_arn(&self, protection_id: &str) -> String {
        let reply = self
            .call(
                "DescribeProtection",
                &json!({ "ProtectionId": protection_id }),
            )
            .await;
        reply
            .body
            .get("Protection")
            .and_then(|protection| protection.get("ProtectionArn"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case DescribeProtection: no ProtectionArn in {reply:?}"))
            .to_string()
    }
}

/// A request that breaks one of the model's rules, and the inputs the refusal has to
/// name.
struct Refused {
    case: &'static str,
    operation: &'static str,
    request: fn() -> Value,
    /// The refused inputs, comma-separated, as the message lists them.
    inputs: &'static str,
}

/// A name one character past `ProtectionName`'s `@length(max: 128)`.
fn over_long_name() -> String {
    "n".repeat(129)
}

/// The `ValidationExceptionReason` a refusal that blames one input member classifies
/// itself with — the model's `FIELD_VALIDATION_FAILED`, as against its only other value
/// `OTHER`.
const FIELD_VALIDATION_FAILED: &str = "FIELD_VALIDATION_FAILED";

/// How this service states that a protection group's `Pattern`/`ResourceType`/`Members`
/// combination is not one it accepts.
const PROTECTION_GROUP_FIELDS_MESSAGE: &str =
    "One or more protection group input fields are incorrect.";

/// The `ValidationExceptionField$message` for a member the group's `Pattern` forbids.
const FIELD_UNEXPECTED_MESSAGE: &str = "This field should not be defined for this pattern";

/// The `ValidationExceptionField$message` for a member the group's `Pattern` requires
/// and the request left unset.
const FIELD_MISSING_MESSAGE: &str = "This field should be defined for this pattern";

/// The `ValidationExceptionField$message` for a `Members` list that names a resource the
/// account does not protect.
const MEMBER_UNPROTECTED_MESSAGE: &str =
    "This field should name only resources that this account protects";

/// How this service states that a `NextToken` is not one it issued.
const INVALID_PAGINATION_TOKEN_MESSAGE: &str = "Invalid pagination token.";

/// How this service states that a resource cannot carry more tags, at the ceiling the
/// modelled `TagList` bound (`len 0..200`) puts on what a read can report.
const TAG_CEILING_MESSAGE: &str = "The resource cannot carry more than 200 tags.";

/// How this service states that the submitted contact list leaves the Shield Response
/// Team no number to call while proactive engagement is enabled.
const MISSING_PHONE_NUMBER_MESSAGE: &str = "The emergency contact list must include at least \
     one phone number while proactive engagement is enabled.";

/// One refusal that blames particular input members: the request that draws it, and the
/// `fields` entries it has to carry, in body order.
struct FieldRefusal {
    case: &'static str,
    request: fn() -> Value,
    fields: &'static [(&'static str, &'static str)],
}

#[tokio::test]
async fn a_protection_group_field_refusal_names_each_member_at_fault() {
    // `CreateProtectionGroupRequest$ResourceType` and `$Members` each document the same
    // rule from both sides — "You must set this when you set Pattern to <P> and you must
    // not set it for any other Pattern setting" — and a `Members` entry has to name a
    // resource the account protects. The table spans every way those rules can be
    // broken, each member at fault on its own and both at once, over all three
    // `ProtectionGroupPattern` values and several `ProtectedResourceType` values.
    let table: &[FieldRefusal] = &[
        FieldRefusal {
            case: "a resource type with the pattern that includes every protected resource",
            request: || {
                json!({
                    "ProtectionGroupId": "faulted-group",
                    "Aggregation": "SUM",
                    "Pattern": "ALL",
                    "ResourceType": "ELASTIC_IP_ALLOCATION",
                })
            },
            fields: &[("ResourceType", FIELD_UNEXPECTED_MESSAGE)],
        },
        FieldRefusal {
            case: "a resource type with an explicit membership",
            request: || {
                json!({
                    "ProtectionGroupId": "faulted-group",
                    "Aggregation": "MEAN",
                    "Pattern": "ARBITRARY",
                    "Members": [PROTECTED_RESOURCE_ARN],
                    "ResourceType": "GLOBAL_ACCELERATOR",
                })
            },
            fields: &[("ResourceType", FIELD_UNEXPECTED_MESSAGE)],
        },
        FieldRefusal {
            case: "a by-resource-type group with no resource type",
            request: || {
                json!({
                    "ProtectionGroupId": "faulted-group",
                    "Aggregation": "MAX",
                    "Pattern": "BY_RESOURCE_TYPE",
                })
            },
            fields: &[("ResourceType", FIELD_MISSING_MESSAGE)],
        },
        FieldRefusal {
            case: "members with the pattern that includes every protected resource",
            request: || {
                json!({
                    "ProtectionGroupId": "faulted-group",
                    "Aggregation": "SUM",
                    "Pattern": "ALL",
                    "Members": [PROTECTED_RESOURCE_ARN],
                })
            },
            fields: &[("Members", FIELD_UNEXPECTED_MESSAGE)],
        },
        FieldRefusal {
            case: "members with a by-resource-type group",
            request: || {
                json!({
                    "ProtectionGroupId": "faulted-group",
                    "Aggregation": "MAX",
                    "Pattern": "BY_RESOURCE_TYPE",
                    "ResourceType": "ROUTE_53_HOSTED_ZONE",
                    "Members": [PROTECTED_RESOURCE_ARN],
                })
            },
            fields: &[("Members", FIELD_UNEXPECTED_MESSAGE)],
        },
        FieldRefusal {
            case: "an explicit membership with no members member",
            request: || {
                json!({
                    "ProtectionGroupId": "faulted-group",
                    "Aggregation": "MEAN",
                    "Pattern": "ARBITRARY",
                })
            },
            fields: &[("Members", FIELD_MISSING_MESSAGE)],
        },
        FieldRefusal {
            case: "an explicit membership with an empty members list",
            request: || {
                json!({
                    "ProtectionGroupId": "faulted-group",
                    "Aggregation": "SUM",
                    "Pattern": "ARBITRARY",
                    "Members": [],
                })
            },
            fields: &[("Members", FIELD_MISSING_MESSAGE)],
        },
        FieldRefusal {
            case: "both members defined where the pattern forbids both",
            request: || {
                json!({
                    "ProtectionGroupId": "faulted-group",
                    "Aggregation": "SUM",
                    "Pattern": "ALL",
                    "ResourceType": "CLOUDFRONT_DISTRIBUTION",
                    "Members": [PROTECTED_RESOURCE_ARN],
                })
            },
            fields: &[
                ("ResourceType", FIELD_UNEXPECTED_MESSAGE),
                ("Members", FIELD_UNEXPECTED_MESSAGE),
            ],
        },
        FieldRefusal {
            case: "a by-resource-type group that named members instead of its type",
            request: || {
                json!({
                    "ProtectionGroupId": "faulted-group",
                    "Aggregation": "MAX",
                    "Pattern": "BY_RESOURCE_TYPE",
                    "Members": [PROTECTED_RESOURCE_ARN],
                })
            },
            fields: &[
                ("ResourceType", FIELD_MISSING_MESSAGE),
                ("Members", FIELD_UNEXPECTED_MESSAGE),
            ],
        },
        FieldRefusal {
            case: "an explicit membership naming a resource the account does not protect",
            request: || {
                json!({
                    "ProtectionGroupId": "faulted-group",
                    "Aggregation": "SUM",
                    "Pattern": "ARBITRARY",
                    "Members": [OTHER_RESOURCE_ARN],
                })
            },
            fields: &[("Members", MEMBER_UNPROTECTED_MESSAGE)],
        },
        FieldRefusal {
            case: "an explicit membership mixing a protected and an unprotected resource",
            request: || {
                json!({
                    "ProtectionGroupId": "faulted-group",
                    "Aggregation": "MEAN",
                    "Pattern": "ARBITRARY",
                    "Members": [PROTECTED_RESOURCE_ARN, OTHER_RESOURCE_ARN],
                })
            },
            fields: &[("Members", MEMBER_UNPROTECTED_MESSAGE)],
        },
    ];

    // One account that protects exactly one resource, so a member naming it is held and
    // a member naming the other is not.
    let emulator = Emulator::new();
    emulator.subscribe().await;
    emulator
        .create_protection("group-member", PROTECTED_RESOURCE_ARN)
        .await;

    for row in table {
        let reply = emulator
            .call("CreateProtectionGroup", &(row.request)())
            .await;
        assert_eq!(
            (
                reply.status,
                reply.error_code(),
                reply.error_message(),
                reply.error_reason()
            ),
            (
                400,
                "InvalidParameterException",
                PROTECTION_GROUP_FIELDS_MESSAGE,
                FIELD_VALIDATION_FAILED
            ),
            "case {}: expected HTTP 400 InvalidParameterException \
             {PROTECTION_GROUP_FIELDS_MESSAGE:?} with reason {FIELD_VALIDATION_FAILED:?}, \
             got {reply:?}",
            row.case
        );
        let expected: Vec<(String, String)> = row
            .fields
            .iter()
            .map(|(name, message)| (name.to_string(), message.to_string()))
            .collect();
        assert_eq!(
            reply.error_fields(),
            Some(expected.clone()),
            "case {}: expected the members at fault to be reported as {expected:?}, got {reply:?}",
            row.case
        );

        // The entries localize the fault by NAME, so neither the sentences nor the names
        // carry a value the caller sent.
        let body = reply.body.to_string();
        for sent in [
            "faulted-group",
            PROTECTED_RESOURCE_ARN,
            OTHER_RESOURCE_ARN,
            "ELASTIC_IP_ALLOCATION",
            "GLOBAL_ACCELERATOR",
            "ROUTE_53_HOSTED_ZONE",
            "CLOUDFRONT_DISTRIBUTION",
        ] {
            assert!(
                !body.contains(sent),
                "case {}: the refusal must carry no value from the request, got {sent:?} in \
                 {body}",
                row.case
            );
        }
    }
}

#[tokio::test]
async fn an_accepted_field_combination_is_not_refused() {
    // The same rule from the side that admits a request: each pattern with exactly the
    // members it documents, including a `BY_RESOURCE_TYPE` group for every value the
    // model's `ProtectedResourceType` declares, read from this service's own catalog so
    // a value the model adds is covered too.
    let emulator = Emulator::new();
    emulator.subscribe().await;
    emulator
        .create_protection("group-member", PROTECTED_RESOURCE_ARN)
        .await;

    let mut accepted: Vec<(String, Value)> = vec![
        (
            "every protected resource of the account".to_string(),
            json!({ "Aggregation": "SUM", "Pattern": "ALL" }),
        ),
        (
            "an explicit membership of one protected resource".to_string(),
            json!({
                "Aggregation": "MEAN",
                "Pattern": "ARBITRARY",
                "Members": [PROTECTED_RESOURCE_ARN],
            }),
        ),
    ];
    for resource_type in PROTECTED_RESOURCE_TYPE_VALUES {
        accepted.push((
            format!("every protected {resource_type}"),
            json!({
                "Aggregation": "MAX",
                "Pattern": "BY_RESOURCE_TYPE",
                "ResourceType": resource_type,
            }),
        ));
    }

    for (index, (case, request)) in accepted.iter().enumerate() {
        let group_id = format!("accepted-{index}");
        let mut request = request.clone();
        request["ProtectionGroupId"] = json!(group_id);
        let reply = emulator.call("CreateProtectionGroup", &request).await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (200, ""),
            "case {case}: expected HTTP 200, got {reply:?}"
        );

        // Read back through a separate request: an admitted combination is persisted as
        // it was sent.
        let reply = emulator
            .call(
                "DescribeProtectionGroup",
                &json!({ "ProtectionGroupId": group_id }),
            )
            .await;
        let group = reply
            .body
            .get("ProtectionGroup")
            .unwrap_or_else(|| panic!("case {case}: no ProtectionGroup in {reply:?}"));
        assert_eq!(
            (
                group.get("Pattern").and_then(Value::as_str),
                group.get("ResourceType").and_then(Value::as_str),
                group.get("Aggregation").and_then(Value::as_str),
            ),
            (
                request.get("Pattern").and_then(Value::as_str),
                request.get("ResourceType").and_then(Value::as_str),
                request.get("Aggregation").and_then(Value::as_str),
            ),
            "case {case}: the read did not report the combination the create was given, \
             got {reply:?}"
        );
    }
}

/// One refusal another operation of this service raises against a named member: the
/// case, the operation, the request, the top-level sentence and the `fields` entries.
type MemberRefusal<'a> = (
    &'a str,
    &'a str,
    Value,
    &'a str,
    Vec<(&'a str, &'a str)>,
);

#[tokio::test]
async fn every_per_field_refusal_of_this_service_names_its_member() {
    // The rule is the exception's, not one operation's: wherever this service raises
    // `InvalidParameterException` because a particular input member is at fault, the
    // structured members carry the diagnosis. The table spans the operations that do so,
    // the members they blame (one member, two at once, a top-level member and a list),
    // and both of the ways the pair of `DescribeProtection`'s addresses can be wrong.
    let emulator = Emulator::new();
    emulator.subscribe().await;
    let protection_id = emulator
        .create_protection("tagged", PROTECTED_RESOURCE_ARN)
        .await;
    let protection_arn = emulator.protection_arn(&protection_id).await;

    // The resource carries as many tags as a read can report, so one more is past the
    // ceiling.
    let to_the_ceiling: Vec<Value> = (0..200)
        .map(|index| json!({ "Key": format!("key-{index}"), "Value": format!("value-{index}") }))
        .collect();
    let reply = emulator
        .call(
            "TagResource",
            &json!({ "ResourceARN": protection_arn, "Tags": to_the_ceiling }),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "case tags up to the ceiling: expected HTTP 200, got {reply:?}"
    );
    // Proactive engagement enabled, which is what makes a contact list with no phone
    // number a refusal.
    emulator.enable_proactive_engagement();

    let table: Vec<MemberRefusal<'_>> = vec![
        (
            "both of DescribeProtection's addresses",
            "DescribeProtection",
            json!({ "ProtectionId": protection_id, "ResourceArn": PROTECTED_RESOURCE_ARN }),
            BOTH_ADDRESSES_MESSAGE,
            vec![
                ("protectionId", BOTH_ADDRESSES_MESSAGE),
                ("resourceArn", BOTH_ADDRESSES_MESSAGE),
            ],
        ),
        (
            "neither of DescribeProtection's addresses",
            "DescribeProtection",
            json!({}),
            NO_ADDRESS_MESSAGE,
            vec![
                ("protectionId", NO_ADDRESS_MESSAGE),
                ("resourceArn", NO_ADDRESS_MESSAGE),
            ],
        ),
        (
            "a pagination token ListAttacks did not issue",
            "ListAttacks",
            json!({ "NextToken": "not-a-token-this-service-issued" }),
            INVALID_PAGINATION_TOKEN_MESSAGE,
            vec![("NextToken", INVALID_PAGINATION_TOKEN_MESSAGE)],
        ),
        (
            "one tag past the resource's ceiling",
            "TagResource",
            json!({
                "ResourceARN": protection_arn,
                "Tags": [{ "Key": "one-too-many", "Value": "x" }],
            }),
            TAG_CEILING_MESSAGE,
            vec![("Tags", TAG_CEILING_MESSAGE)],
        ),
        (
            "a contact list with no phone number while proactive engagement is enabled",
            "UpdateEmergencyContactSettings",
            json!({ "EmergencyContactList": [{ "EmailAddress": "oncall@example.com" }] }),
            MISSING_PHONE_NUMBER_MESSAGE,
            vec![("EmergencyContactList", MISSING_PHONE_NUMBER_MESSAGE)],
        ),
    ];

    for (case, operation, request, message, fields) in &table {
        let reply = emulator.call(operation, request).await;
        assert_eq!(
            (
                reply.status,
                reply.error_code(),
                reply.error_message(),
                reply.error_reason()
            ),
            (
                400,
                "InvalidParameterException",
                *message,
                FIELD_VALIDATION_FAILED
            ),
            "case {case}: expected HTTP 400 InvalidParameterException {message:?} with \
             reason {FIELD_VALIDATION_FAILED:?}, got {reply:?}"
        );
        let expected: Vec<(String, String)> = fields
            .iter()
            .map(|(name, message)| (name.to_string(), message.to_string()))
            .collect();
        assert_eq!(
            reply.error_fields(),
            Some(expected.clone()),
            "case {case}: expected the members at fault to be reported as {expected:?}, \
             got {reply:?}"
        );
    }
}

#[tokio::test]
async fn a_model_rule_refusal_names_only_the_inputs_it_refused() {
    // The table spans the constraint kinds the model uses (required, length minimum,
    // length maximum, pattern, range, enum), the operations that carry them, top-level
    // and nested members, and one refused input against two.
    let table: &[Refused] = &[
        Refused {
            case: "a name outside ProtectionName's pattern",
            operation: "CreateProtection",
            request: || json!({ "Name": "bad!name@@", "ResourceArn": PROTECTED_RESOURCE_ARN }),
            inputs: "name",
        },
        Refused {
            case: "an empty name, below ProtectionName's length minimum",
            operation: "CreateProtection",
            request: || json!({ "Name": "", "ResourceArn": PROTECTED_RESOURCE_ARN }),
            inputs: "name",
        },
        Refused {
            case: "a name past ProtectionName's length maximum",
            operation: "CreateProtection",
            request: || {
                json!({ "Name": over_long_name(), "ResourceArn": PROTECTED_RESOURCE_ARN })
            },
            inputs: "name",
        },
        Refused {
            case: "an absent required name",
            operation: "CreateProtection",
            request: || json!({ "ResourceArn": PROTECTED_RESOURCE_ARN }),
            inputs: "name",
        },
        Refused {
            case: "a resource arn outside ResourceArn's pattern",
            operation: "CreateProtection",
            request: || json!({ "Name": "well-formed", "ResourceArn": "not-an-arn" }),
            inputs: "resourceArn",
        },
        Refused {
            case: "both of CreateProtection's inputs refused at once",
            operation: "CreateProtection",
            request: || json!({ "Name": "bad!name@@", "ResourceArn": "not-an-arn" }),
            inputs: "name, resourceArn",
        },
        Refused {
            case: "a tag key below TagKey's length minimum, nested under tags",
            operation: "CreateProtection",
            request: || {
                json!({
                    "Name": "well-formed",
                    "ResourceArn": PROTECTED_RESOURCE_ARN,
                    "Tags": [{ "Key": "", "Value": "v" }],
                })
            },
            inputs: "tags",
        },
        Refused {
            case: "a protection id shorter than ProtectionId's fixed length",
            operation: "DescribeProtection",
            request: || json!({ "ProtectionId": "too-short" }),
            inputs: "protectionId",
        },
        Refused {
            case: "an absent required attack id",
            operation: "DescribeAttack",
            request: || json!({}),
            inputs: "attackId",
        },
        Refused {
            case: "a protection group id outside ProtectionGroupId's pattern",
            operation: "CreateProtectionGroup",
            request: || {
                json!({
                    "ProtectionGroupId": "not a group id!",
                    "Aggregation": "SUM",
                    "Pattern": "ALL",
                })
            },
            inputs: "protectionGroupId",
        },
        Refused {
            case: "an aggregation outside ProtectionGroupAggregation's values",
            operation: "CreateProtectionGroup",
            request: || {
                json!({
                    "ProtectionGroupId": "group",
                    "Aggregation": "AVERAGE-OF-EVERYTHING",
                    "Pattern": "ALL",
                })
            },
            inputs: "aggregation",
        },
        Refused {
            case: "a page size past MaxResults' range maximum",
            operation: "ListProtections",
            request: || json!({ "MaxResults": 10_001 }),
            inputs: "maxResults",
        },
        Refused {
            case: "a page size below MaxResults' range minimum",
            operation: "ListProtections",
            request: || json!({ "MaxResults": -1 }),
            inputs: "maxResults",
        },
    ];

    for row in table {
        // A subscribed account, so nothing but the model's own rules can decide the
        // answer.
        let emulator = Emulator::new();
        emulator.subscribe().await;

        let reply = emulator.call(row.operation, &(row.request)()).await;
        let expected = format!("{VALIDATION_MESSAGE_PREFIX}{}", row.inputs);
        assert_eq!(
            (reply.status, reply.error_code(), reply.error_message()),
            (400, "ValidationException", expected.as_str()),
            "case {}: expected HTTP 400 ValidationException {expected:?}, got {reply:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_model_rule_refusal_echoes_no_submitted_value_and_no_model_constraint() {
    // The refusal states which inputs were refused, never what was sent or what the
    // model asks of them: the request's own values must not come back, and neither may
    // the model's regular expressions or bounds.
    let leaks: &[(&str, &str, Value, &[&str])] = &[
        (
            "a name outside ProtectionName's pattern",
            "CreateProtection",
            json!({ "Name": "bad!name@@", "ResourceArn": PROTECTED_RESOURCE_ARN }),
            &["bad!name@@", "a-zA-Z0-9", "regular expression", "satisfy"],
        ),
        (
            "a resource arn outside ResourceArn's pattern",
            "CreateProtection",
            json!({ "Name": "well-formed", "ResourceArn": "arn:not-aws:x" }),
            &["arn:not-aws:x", "^arn:aws", "regular expression"],
        ),
        (
            "a tag key below TagKey's length minimum",
            "CreateProtection",
            json!({
                "Name": "well-formed",
                "ResourceArn": PROTECTED_RESOURCE_ARN,
                "Tags": [{ "Key": "", "Value": "caller-chosen" }],
            }),
            &["caller-chosen", "length", "128"],
        ),
        (
            "a page size past MaxResults' range maximum",
            "ListProtections",
            json!({ "MaxResults": 10_001 }),
            &["10001", "10000", "value greater", "value less"],
        ),
    ];

    for (case, operation, request, forbidden) in leaks {
        let emulator = Emulator::new();
        emulator.subscribe().await;

        let reply = emulator.call(operation, request).await;
        assert_eq!(
            reply.error_code(),
            "ValidationException",
            "case {case}: expected the model-rule refusal, got {reply:?}"
        );
        for fragment in *forbidden {
            assert!(
                !reply.error_message().contains(fragment),
                "case {case}: the refusal must not carry {fragment:?}, got {:?}",
                reply.error_message()
            );
        }
    }
}

#[tokio::test]
async fn a_value_on_a_published_bound_is_not_refused() {
    // The same rule from the other side: `MaxResults` is modelled `@range(min: 0, max:
    // 10000)` and `ProtectionName` `@length(min: 1, max: 128)`, so each bound itself is
    // admitted — only a value past it is refused.
    let emulator = Emulator::new();
    emulator.subscribe().await;

    for max_results in [0, 1, 20, 9_999, 10_000] {
        let reply = emulator
            .call("ListProtections", &json!({ "MaxResults": max_results }))
            .await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (200, ""),
            "case MaxResults {max_results}: expected HTTP 200, got {reply:?}"
        );
    }

    for (case, name, resource_arn) in [
        ("shortest name", "n".to_string(), PROTECTED_RESOURCE_ARN),
        ("longest name", "n".repeat(128), OTHER_RESOURCE_ARN),
    ] {
        let reply = emulator
            .call(
                "CreateProtection",
                &json!({ "Name": name, "ResourceArn": resource_arn }),
            )
            .await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (200, ""),
            "case {case}: expected HTTP 200, got {reply:?}"
        );
    }
}

#[tokio::test]
async fn describe_protection_states_the_identifier_pair_requirement() {
    let emulator = Emulator::new();
    emulator.subscribe().await;
    let held = emulator
        .create_protection("pair", PROTECTED_RESOURCE_ARN)
        .await;

    // Naming both addresses is refused whichever values they carry — a held pair, an
    // unheld one, a mix — and the sentence is the same every time, carrying neither.
    let both: &[(&str, Value)] = &[
        (
            "both addresses of a held protection",
            json!({ "ProtectionId": held, "ResourceArn": PROTECTED_RESOURCE_ARN }),
        ),
        (
            "both addresses, neither held",
            json!({ "ProtectionId": UNHELD_PROTECTION_ID, "ResourceArn": OTHER_RESOURCE_ARN }),
        ),
        (
            "a held id with another resource's arn",
            json!({ "ProtectionId": held, "ResourceArn": OTHER_RESOURCE_ARN }),
        ),
    ];
    for (case, request) in both {
        let reply = emulator.call("DescribeProtection", request).await;
        assert_eq!(
            (reply.status, reply.error_code(), reply.error_message()),
            (400, "InvalidParameterException", BOTH_ADDRESSES_MESSAGE),
            "case {case}: expected HTTP 400 InvalidParameterException \
             {BOTH_ADDRESSES_MESSAGE:?}, got {reply:?}"
        );
    }

    // The same pair left unsatisfied from the other side.
    let reply = emulator.call("DescribeProtection", &json!({})).await;
    assert_eq!(
        (reply.status, reply.error_code(), reply.error_message()),
        (400, "InvalidParameterException", NO_ADDRESS_MESSAGE),
        "case neither address: expected HTTP 400 InvalidParameterException \
         {NO_ADDRESS_MESSAGE:?}, got {reply:?}"
    );

    // Exactly one address still reports the stored record, from either side.
    for (case, request) in [
        ("by id", json!({ "ProtectionId": held })),
        ("by arn", json!({ "ResourceArn": PROTECTED_RESOURCE_ARN })),
    ] {
        let reply = emulator.call("DescribeProtection", &request).await;
        let protection = reply
            .body
            .get("Protection")
            .unwrap_or_else(|| panic!("case {case}: no Protection in {reply:?}"));
        assert_eq!(
            (
                reply.status,
                protection.get("Id").and_then(Value::as_str),
                protection.get("ResourceArn").and_then(Value::as_str),
            ),
            (200, Some(held.as_str()), Some(PROTECTED_RESOURCE_ARN)),
            "case {case}: expected the stored protection, got {reply:?}"
        );
    }
}

/// Each way an account can hold no active subscription: it never took one on, its
/// subscription has just run past its end, or it lapsed long ago. `None` is the account
/// that never subscribed; `Some(d)` is a subscription whose end is `d` in the past.
const UNSUBSCRIBED_STATES: &[(&str, Option<Duration>)] = &[
    ("never subscribed", None),
    ("lapsed a second ago", Some(Duration::from_secs(1))),
    (
        "lapsed a year ago",
        Some(Duration::from_secs(365 * 24 * 60 * 60)),
    ),
];

#[tokio::test]
async fn addressing_a_protection_without_a_subscription_reports_the_account_level_absence() {
    // A protection is something a subscription grants, so the absence an unsubscribed
    // account reports is the account's, not the protection's — whichever address the
    // call used, and whether or not the id was ever held.
    for (state_case, lapsed_by) in UNSUBSCRIBED_STATES {
        for (call_case, operation, request) in [
            (
                "DescribeProtection by id",
                "DescribeProtection",
                json!({ "ProtectionId": UNHELD_PROTECTION_ID }),
            ),
            (
                "DescribeProtection by arn",
                "DescribeProtection",
                json!({ "ResourceArn": PROTECTED_RESOURCE_ARN }),
            ),
            (
                "DeleteProtection",
                "DeleteProtection",
                json!({ "ProtectionId": UNHELD_PROTECTION_ID }),
            ),
        ] {
            let emulator = Emulator::new();
            if let Some(lapsed_by) = lapsed_by {
                emulator.subscribe().await;
                emulator.set_subscription_end(SystemTime::now() - *lapsed_by);
            }

            let reply = emulator.call(operation, &request).await;
            assert_eq!(
                (reply.status, reply.error_code(), reply.error_message()),
                (
                    400,
                    "ResourceNotFoundException",
                    SUBSCRIPTION_NOT_FOUND_MESSAGE
                ),
                "case {call_case} on an account {state_case}: expected HTTP 400 \
                 ResourceNotFoundException {SUBSCRIPTION_NOT_FOUND_MESSAGE:?}, got {reply:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_missing_protection_is_reported_by_its_resource_class() {
    let emulator = Emulator::new();
    emulator.subscribe().await;
    let held = emulator
        .create_protection("released", PROTECTED_RESOURCE_ARN)
        .await;

    // A well-formed address that resolves to nothing: the refusal names the resource
    // class, never the identifier the call carried.
    let missing: &[(&str, &str, Value)] = &[
        (
            "describe an unheld id",
            "DescribeProtection",
            json!({ "ProtectionId": UNHELD_PROTECTION_ID }),
        ),
        (
            "describe an unprotected resource",
            "DescribeProtection",
            json!({ "ResourceArn": OTHER_RESOURCE_ARN }),
        ),
        (
            "delete an unheld id",
            "DeleteProtection",
            json!({ "ProtectionId": UNHELD_PROTECTION_ID }),
        ),
    ];
    for (case, operation, request) in missing {
        let reply = emulator.call(operation, request).await;
        assert_eq!(
            (reply.status, reply.error_code(), reply.error_message()),
            (400, "ResourceNotFoundException", PROTECTION_NOT_FOUND_MESSAGE),
            "case {case}: expected HTTP 400 ResourceNotFoundException \
             {PROTECTION_NOT_FOUND_MESSAGE:?}, got {reply:?}"
        );
        assert!(
            !reply.error_message().contains(UNHELD_PROTECTION_ID)
                && !reply.error_message().contains(OTHER_RESOURCE_ARN),
            "case {case}: the refusal must carry no address from the request, got {:?}",
            reply.error_message()
        );
    }

    // The same sentence once a protection that WAS held is released: the record is gone
    // from the store, so both of its addresses report the class.
    let reply = emulator
        .call("DeleteProtection", &json!({ "ProtectionId": held }))
        .await;
    assert_eq!(
        reply.status, 200,
        "case release the held protection: expected HTTP 200, got {reply:?}"
    );
    for (case, request) in [
        ("released id", json!({ "ProtectionId": held })),
        (
            "released resource",
            json!({ "ResourceArn": PROTECTED_RESOURCE_ARN }),
        ),
    ] {
        let reply = emulator.call("DescribeProtection", &request).await;
        assert_eq!(
            (reply.status, reply.error_code(), reply.error_message()),
            (400, "ResourceNotFoundException", PROTECTION_NOT_FOUND_MESSAGE),
            "case {case}: expected HTTP 400 ResourceNotFoundException \
             {PROTECTION_NOT_FOUND_MESSAGE:?}, got {reply:?}"
        );
    }
}
