// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `UpdateThingType`: which parts of a registered thing type
//! one changes, which it leaves alone, and how it refuses a request it cannot apply.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value, and every change is read
//! back through `DescribeThingType`.
//!
//! The rules under test:
//!
//! * `UpdateThingType` is bound to HTTP `PATCH` on `/thing-types/{thingTypeName}` with
//!   `thingTypeProperties` and every member of it OPTIONAL, so each member the request
//!   PROVIDES is what the type carries afterwards and each member it OMITS leaves the
//!   stored one exactly as it was. A provided list REPLACES the stored one rather than
//!   merging into it, and a provided EMPTY list leaves the type with no such entries.
//! * A `thingTypeName` the account holds no type under is the modelled
//!   `ResourceNotFoundException` (404), worded the way every operation addressing one
//!   thing type words that miss, and the registry is left untouched.
//! * The ceiling on how many entries one of a type's property lists may carry is the one
//!   rule both writers of a thing type enforce, so an update past it is the modelled
//!   `InvalidRequestException` (400) naming the member and the limit, and the type keeps
//!   the configuration it had.
//! * An update changes the CONFIGURATION only: the type's id, its ARN, its creation
//!   instant and its deprecation state are not members of the request and survive it.
//!
//! The table spans the class the first rule generalises over — which members the request
//! provides (each one alone, all of them, none of them), whether a provided list is
//! longer, shorter or empty, which members of a `PropagatingAttribute` the entries carry,
//! and whether the type was created with any properties at all — rather than one recorded
//! request.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::MAX_THING_TYPE_PROPERTY_ENTRIES;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use super::{signed_for_iot, ACCOUNT_ID, REGION};

/// The `x-amzn-errortype` a modelled not-found carries.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The `x-amzn-errortype` this service names its legacy bad-request shape with: the shape
/// name qualified with the API doc namespace.
const INVALID_REQUEST: &str = "InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/";

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// The `message` member a modelled error carries.
    fn message(&self) -> Option<&str> {
        self.body.get("message").and_then(Value::as_str)
    }

    /// The `thingTypeProperties` the read reports, exactly as the wire carries it. An
    /// absent member is absent from the object: that is how a type carrying no entries for
    /// it reports, so comparing the whole object compares every member at once.
    fn properties(&self) -> Value {
        self.body
            .get("thingTypeProperties")
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// A string member of the body, or `None` when it is absent or not a string.
    fn text(&self, name: &str) -> Option<&str> {
        self.body.get(name).and_then(Value::as_str)
    }

    /// A member of `thingTypeMetadata`, or `Value::Null` when the read reports none.
    fn metadata(&self, name: &str) -> Value {
        self.body
            .get("thingTypeMetadata")
            .and_then(|metadata| metadata.get(name))
            .cloned()
            .unwrap_or(Value::Null)
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

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot(REGION))
            .header("content-type", "application/json")
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

    /// `CreateThingType`, carrying `properties` as its `thingTypeProperties` when the case
    /// states any.
    async fn create_type(&self, thing_type_name: &str, properties: &Value) -> Reply {
        let body = if properties.is_null() {
            json!({})
        } else {
            json!({ "thingTypeProperties": properties })
        };
        self.call(
            "POST",
            &format!("/thing-types/{thing_type_name}"),
            Some(&body),
        )
        .await
    }

    /// `UpdateThingType` with `body` as the whole request payload.
    async fn update_type(&self, thing_type_name: &str, body: &Value) -> Reply {
        self.call(
            "PATCH",
            &format!("/thing-types/{thing_type_name}"),
            Some(body),
        )
        .await
    }

    async fn describe_type(&self, thing_type_name: &str) -> Reply {
        self.call("GET", &format!("/thing-types/{thing_type_name}"), None)
            .await
    }

    async fn deprecate_type(&self, thing_type_name: &str) -> Reply {
        self.call(
            "POST",
            &format!("/thing-types/{thing_type_name}/deprecate"),
            Some(&json!({})),
        )
        .await
    }
}

/// `count` searchable attribute names, for the cases that drive a list to the ceiling.
fn searchable_attributes(count: usize) -> Value {
    Value::Array(
        (0..count)
            .map(|index| Value::String(format!("attribute{index}")))
            .collect(),
    )
}

/// `count` propagating attributes, for the cases that drive that list to the ceiling.
fn propagating_attributes(count: usize) -> Value {
    Value::Array(
        (0..count)
            .map(|index| {
                json!({
                    "userPropertyKey": format!("key{index}"),
                    "thingAttribute": format!("attribute{index}"),
                })
            })
            .collect(),
    )
}

/// The properties most cases create their type with: one of every member, so a case that
/// states one member in its update proves the other two survived.
fn created_properties() -> Value {
    json!({
        "thingTypeDescription": "sensors on the north line",
        "searchableAttributes": ["serial", "floor"],
        "mqtt5Configuration": {
            "propagatingAttributes": [
                { "userPropertyKey": "serial", "thingAttribute": "serial" }
            ]
        }
    })
}

/// One case of the member-wise update rule.
struct Case {
    /// What the case proves, named in every assertion.
    label: &'static str,
    /// The `thingTypeProperties` of the create, or `Value::Null` for a create that carries
    /// none at all.
    created: Value,
    /// The WHOLE `UpdateThingType` request body.
    update: Value,
    /// The `thingTypeProperties` a later `DescribeThingType` must report.
    expected: Value,
}

#[tokio::test]
async fn an_update_applies_the_members_it_states_and_preserves_the_rest() {
    let cases = vec![
        Case {
            label: "the MQTT 5 configuration alone replaces that list and preserves the rest",
            created: created_properties(),
            update: json!({
                "thingTypeProperties": {
                    "mqtt5Configuration": {
                        "propagatingAttributes": [
                            { "userPropertyKey": "floor", "thingAttribute": "floor" },
                            { "userPropertyKey": "client", "connectionAttribute": "iot:ClientId" }
                        ]
                    }
                }
            }),
            expected: json!({
                "thingTypeDescription": "sensors on the north line",
                "searchableAttributes": ["serial", "floor"],
                "mqtt5Configuration": {
                    "propagatingAttributes": [
                        { "userPropertyKey": "floor", "thingAttribute": "floor" },
                        { "userPropertyKey": "client", "connectionAttribute": "iot:ClientId" }
                    ]
                }
            }),
        },
        Case {
            label: "the description alone replaces the description and preserves the rest",
            created: created_properties(),
            update: json!({
                "thingTypeProperties": { "thingTypeDescription": "sensors on the south line" }
            }),
            expected: json!({
                "thingTypeDescription": "sensors on the south line",
                "searchableAttributes": ["serial", "floor"],
                "mqtt5Configuration": {
                    "propagatingAttributes": [
                        { "userPropertyKey": "serial", "thingAttribute": "serial" }
                    ]
                }
            }),
        },
        Case {
            label: "a provided searchable-attribute list REPLACES the stored one",
            created: created_properties(),
            update: json!({
                "thingTypeProperties": { "searchableAttributes": ["bay"] }
            }),
            expected: json!({
                "thingTypeDescription": "sensors on the north line",
                "searchableAttributes": ["bay"],
                "mqtt5Configuration": {
                    "propagatingAttributes": [
                        { "userPropertyKey": "serial", "thingAttribute": "serial" }
                    ]
                }
            }),
        },
        Case {
            label: "every member at once",
            created: created_properties(),
            update: json!({
                "thingTypeProperties": {
                    "thingTypeDescription": "rebuilt",
                    "searchableAttributes": ["serial", "floor", "bay"],
                    "mqtt5Configuration": {
                        "propagatingAttributes": [
                            { "connectionAttribute": "iot:Connection.Thing.ThingName" }
                        ]
                    }
                }
            }),
            expected: json!({
                "thingTypeDescription": "rebuilt",
                "searchableAttributes": ["serial", "floor", "bay"],
                "mqtt5Configuration": {
                    "propagatingAttributes": [
                        { "connectionAttribute": "iot:Connection.Thing.ThingName" }
                    ]
                }
            }),
        },
        Case {
            label: "a properties object stating no member changes nothing",
            created: created_properties(),
            update: json!({ "thingTypeProperties": {} }),
            expected: created_properties(),
        },
        Case {
            label: "a request carrying no properties at all changes nothing",
            created: created_properties(),
            update: json!({}),
            expected: created_properties(),
        },
        Case {
            label: "an mqtt5Configuration carrying no list states nothing about that list",
            created: created_properties(),
            update: json!({
                "thingTypeProperties": { "mqtt5Configuration": {} }
            }),
            expected: created_properties(),
        },
        Case {
            label: "an empty searchable-attribute list leaves the type with none",
            created: created_properties(),
            update: json!({
                "thingTypeProperties": { "searchableAttributes": [] }
            }),
            expected: json!({
                "thingTypeDescription": "sensors on the north line",
                "mqtt5Configuration": {
                    "propagatingAttributes": [
                        { "userPropertyKey": "serial", "thingAttribute": "serial" }
                    ]
                }
            }),
        },
        Case {
            label: "an empty propagating-attribute list leaves the type with no MQTT 5 \
                    configuration to report",
            created: created_properties(),
            update: json!({
                "thingTypeProperties": { "mqtt5Configuration": { "propagatingAttributes": [] } }
            }),
            expected: json!({
                "thingTypeDescription": "sensors on the north line",
                "searchableAttributes": ["serial", "floor"]
            }),
        },
        Case {
            label: "a type created with no properties takes the ones the update states",
            created: Value::Null,
            update: json!({
                "thingTypeProperties": {
                    "thingTypeDescription": "created bare",
                    "searchableAttributes": ["serial"],
                    "mqtt5Configuration": {
                        "propagatingAttributes": [
                            { "userPropertyKey": "serial", "thingAttribute": "serial" }
                        ]
                    }
                }
            }),
            expected: json!({
                "thingTypeDescription": "created bare",
                "searchableAttributes": ["serial"],
                "mqtt5Configuration": {
                    "propagatingAttributes": [
                        { "userPropertyKey": "serial", "thingAttribute": "serial" }
                    ]
                }
            }),
        },
    ];

    let emulator = Emulator::new();
    for (index, case) in cases.iter().enumerate() {
        let label = case.label;
        let thing_type_name = format!("update-case-{index}");

        let created = emulator.create_type(&thing_type_name, &case.created).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: CreateThingType must succeed, got {created:?}"
        );

        let updated = emulator.update_type(&thing_type_name, &case.update).await;
        assert_eq!(
            updated.status, 200,
            "case {label:?}: UpdateThingType must succeed, got {updated:?}"
        );

        let described = emulator.describe_type(&thing_type_name).await;
        assert_eq!(
            described.status, 200,
            "case {label:?}: DescribeThingType must succeed, got {described:?}"
        );
        assert_eq!(
            described.properties(),
            case.expected,
            "case {label:?}: expected thingTypeProperties {:?}, got {:?}",
            case.expected,
            described.properties()
        );
    }
}

#[tokio::test]
async fn an_update_of_a_name_the_registry_holds_no_type_under_is_not_found() {
    // The names span the class the rule generalises over: a plain name nobody created, one
    // carrying every character class the `ThingTypeName` pattern admits, one at the
    // modelled maximum length of 128, and one differing only in case from a type that DOES
    // exist — the registry keys a type by its name exactly.
    let registered = "NorthLine";
    let missing_names = vec![
        "absent-type".to_string(),
        "A:b_c-1".to_string(),
        "n".repeat(128),
        registered.to_lowercase(),
    ];

    let emulator = Emulator::new();
    let created = emulator
        .create_type(registered, &created_properties())
        .await;
    assert_eq!(
        created.status, 200,
        "the registered type must be created, got {created:?}"
    );

    for name in &missing_names {
        let refused = emulator
            .update_type(
                name,
                &json!({ "thingTypeProperties": { "thingTypeDescription": "whatever" } }),
            )
            .await;
        assert_eq!(
            refused.status, 404,
            "case {name:?}: UpdateThingType of an unregistered name must be refused 404, \
             got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(NOT_FOUND),
            "case {name:?}: expected ResourceNotFoundException, got {refused:?}"
        );
        let expected_message = format!("Thing Type {ACCOUNT_ID}:{name} not found.");
        assert_eq!(
            refused.message(),
            Some(expected_message.as_str()),
            "case {name:?}: expected message {expected_message:?}, got {:?}",
            refused.message()
        );

        // The refused update recorded nothing: the name is still one the registry holds no
        // type under.
        let described = emulator.describe_type(name).await;
        assert_eq!(
            described.status, 404,
            "case {name:?}: a refused update must register nothing, got {described:?}"
        );
    }

    // The type that does exist was not touched by any of the refusals above.
    let described = emulator.describe_type(registered).await;
    assert_eq!(
        described.properties(),
        created_properties(),
        "the registered type must keep its properties, got {:?}",
        described.properties()
    );
}

/// One case of the per-list ceiling: which member the update fills, how many entries it
/// sends, and whether that many is admitted.
struct CeilingCase {
    /// The member path the refusal names.
    member: &'static str,
    /// How many entries the update sends.
    count: usize,
    /// Whether a list of that many entries is admitted.
    accepted: bool,
}

#[tokio::test]
async fn an_update_past_the_property_list_ceiling_is_refused_and_changes_nothing() {
    // Each published bound and one past it, for both bounded lists.
    let cases = [
        CeilingCase {
            member: "searchableAttributes",
            count: MAX_THING_TYPE_PROPERTY_ENTRIES,
            accepted: true,
        },
        CeilingCase {
            member: "searchableAttributes",
            count: MAX_THING_TYPE_PROPERTY_ENTRIES + 1,
            accepted: false,
        },
        CeilingCase {
            member: "mqtt5Configuration.propagatingAttributes",
            count: MAX_THING_TYPE_PROPERTY_ENTRIES,
            accepted: true,
        },
        CeilingCase {
            member: "mqtt5Configuration.propagatingAttributes",
            count: MAX_THING_TYPE_PROPERTY_ENTRIES + 1,
            accepted: false,
        },
    ];

    let emulator = Emulator::new();
    for (index, case) in cases.iter().enumerate() {
        let CeilingCase {

            member,
            count,
            accepted,
        } = case;
        let label = format!("{member} x{count}");
        let thing_type_name = format!("ceiling-case-{index}");
        let created = emulator
            .create_type(&thing_type_name, &created_properties())
            .await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: CreateThingType must succeed, got {created:?}"
        );

        let body = if *member == "searchableAttributes" {
            json!({ "thingTypeProperties": { "searchableAttributes": searchable_attributes(*count) } })
        } else {
            json!({
                "thingTypeProperties": {
                    "mqtt5Configuration": { "propagatingAttributes": propagating_attributes(*count) }
                }
            })
        };
        let updated = emulator.update_type(&thing_type_name, &body).await;

        if *accepted {
            assert_eq!(
                updated.status, 200,
                "case {label:?}: a list at the ceiling must be admitted, got {updated:?}"
            );
            let described = emulator.describe_type(&thing_type_name).await;
            let stored = described.properties();
            let entries = if *member == "searchableAttributes" {
                stored.get("searchableAttributes").cloned()
            } else {
                stored
                    .get("mqtt5Configuration")
                    .and_then(|configuration| configuration.get("propagatingAttributes"))
                    .cloned()
            };
            assert_eq!(
                entries.as_ref().and_then(Value::as_array).map(Vec::len),
                Some(*count),
                "case {label:?}: expected {count} stored entries, got {entries:?}"
            );
            continue;
        }

        assert_eq!(
            updated.status, 400,
            "case {label:?}: a list past the ceiling must be refused 400, got {updated:?}"
        );
        assert_eq!(
            updated.error_code.as_deref(),
            Some(INVALID_REQUEST),
            "case {label:?}: expected InvalidRequestException, got {updated:?}"
        );
        let expected_message = format!(
            "a thing type's {member} may carry at most {MAX_THING_TYPE_PROPERTY_ENTRIES} \
             entries; this request carries {count}"
        );
        assert_eq!(
            updated.message(),
            Some(expected_message.as_str()),
            "case {label:?}: expected message {expected_message:?}, got {:?}",
            updated.message()
        );

        // A refused update writes nothing: the type still carries what it was created with.
        let described = emulator.describe_type(&thing_type_name).await;
        assert_eq!(
            described.properties(),
            created_properties(),
            "case {label:?}: a refused update must leave the configuration alone, got {:?}",
            described.properties()
        );
    }
}

#[tokio::test]
async fn an_update_changes_the_configuration_alone() {
    let emulator = Emulator::new();
    let thing_type_name = "identity-survives-update";
    let created = emulator
        .create_type(thing_type_name, &created_properties())
        .await;
    assert_eq!(
        created.status, 200,
        "CreateThingType must succeed, got {created:?}"
    );

    // Deprecated first, so the update is asserted against a type carrying every piece of
    // state an update must not move.
    let deprecated = emulator.deprecate_type(thing_type_name).await;
    assert_eq!(
        deprecated.status, 200,
        "DeprecateThingType must succeed, got {deprecated:?}"
    );
    let before = emulator.describe_type(thing_type_name).await;
    assert_eq!(
        before.status, 200,
        "DescribeThingType must succeed, got {before:?}"
    );

    let updated = emulator
        .update_type(
            thing_type_name,
            &json!({ "thingTypeProperties": { "thingTypeDescription": "re-described" } }),
        )
        .await;
    assert_eq!(
        updated.status, 200,
        "UpdateThingType of a deprecated type must succeed, got {updated:?}"
    );

    let after = emulator.describe_type(thing_type_name).await;
    for member in ["thingTypeName", "thingTypeId", "thingTypeArn"] {
        assert_eq!(
            after.text(member),
            before.text(member),
            "case {member:?}: an update must not change it, got {:?} after {:?}",
            after.text(member),
            before.text(member)
        );
    }
    for member in ["creationDate", "deprecated", "deprecationDate"] {
        assert_eq!(
            after.metadata(member),
            before.metadata(member),
            "case {member:?}: an update must not change it, got {:?} after {:?}",
            after.metadata(member),
            before.metadata(member)
        );
    }
    assert_eq!(
        after.metadata("deprecated"),
        Value::Bool(true),
        "the type must still be deprecated, got {:?}",
        after.metadata("deprecated")
    );
    assert_eq!(
        after.properties(),
        json!({
            "thingTypeDescription": "re-described",
            "searchableAttributes": ["serial", "floor"],
            "mqtt5Configuration": {
                "propagatingAttributes": [
                    { "userPropertyKey": "serial", "thingAttribute": "serial" }
                ]
            }
        }),
        "the stated member must be the only one that moved, got {:?}",
        after.properties()
    );
}
