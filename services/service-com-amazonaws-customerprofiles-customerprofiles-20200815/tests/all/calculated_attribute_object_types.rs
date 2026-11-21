// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the object type a calculated attribute definition names.
//!
//! The rule under test: a calculated attribute aggregates an attribute OF a profile object
//! type, so every object type its `AttributeDetails` names — the `ObjectTypeName` part of
//! each `{ObjectTypeName.AttributeName}` element of the expression, and of an attribute item
//! qualified with one — has to be a type the domain already defines. A name the domain does
//! not define is refused `BadRequestException` (400) with the service's sentence, and
//! nothing is stored for it.
//!
//! The rule is asserted over a SET of inputs — which half of `AttributeDetails` carries the
//! name, how many elements the expression has, and whether the type is one the domain
//! defined, the standard profile model every domain holds, or a name nothing defines —
//! rather than for one recorded request. Every case goes through the real router, so what is
//! asserted is the wire behaviour a client sees. The HTTP plumbing and the create helpers
//! are this crate's one test harness, in `tests/all/domain_profile_lifecycle.rs`.

use serde_json::{json, Value};

use crate::calculated_attribute_definitions::{
    attribute_details, create, create_domain_with_object_type, get, OBJECT_TYPE,
};
use crate::domain_profile_lifecycle::{error_code, error_message, Emulator};

/// The domain every case addresses, which defines [`OBJECT_TYPE`] and nothing else.
const DOMAIN: &str = "objtypedom";

/// The standard profile model every domain holds its profiles' own data under — the one
/// object type a domain defines without a `PutProfileObjectType` call, named by every object
/// type field `Target` (`_profile.Address.PostalCode`).
const STANDARD_PROFILE_TYPE: &str = "_profile";

/// The sentence the service refuses an undefined object type with: fixed text whose only
/// variable part is the offending name, taken from the request's own `AttributeDetails`.
fn invalid_object_type(object_type: &str) -> String {
    format!("Invalid object type name: {object_type}")
}

/// RULE, refusal half: an `AttributeDetails` naming an object type the domain does not
/// define is refused 400 with the offending name, and no definition is stored.
#[tokio::test]
async fn a_definition_over_an_undefined_object_type_is_refused_with_that_name() {
    let emulator = Emulator::new();
    create_domain_with_object_type(&emulator, DOMAIN, 30).await;

    // The same defined name in another case, and one character short of it: neither is a
    // type the domain defines, because a type is identified by its name exactly as written.
    let lowercased = OBJECT_TYPE.to_lowercase();
    let truncated = &OBJECT_TYPE[..OBJECT_TYPE.len() - 1];

    // (case, the name the created definition is attempted under, the AttributeDetails, the
    // object type the refusal must name)
    let rows: Vec<(&str, &str, Value, &str)> = vec![
        (
            "an object type nothing ever defined",
            "Unknown_Type",
            attribute_details("Shipments", &["Amount"]),
            "Shipments",
        ),
        (
            "an object type defined in no domain, named beside a defined one",
            "Second_Element_Unknown",
            json!({
                "Attributes": [{ "Name": "Amount" }, { "Name": "Weight" }],
                "Expression": format!("{{{OBJECT_TYPE}.Amount}} - {{Shipments.Weight}}"),
            }),
            "Shipments",
        ),
        (
            "an attribute item qualified with an undefined object type",
            "Qualified_Item_Unknown",
            json!({
                "Attributes": [{ "Name": "Shipments.Weight" }],
                "Expression": "{Shipments.Weight}",
            }),
            "Shipments",
        ),
        (
            "a name that differs from a defined type only in case",
            "Case_Mismatch",
            attribute_details(&lowercased, &["Amount"]),
            &lowercased,
        ),
        (
            "a name that is a prefix of a defined type",
            "Prefix_Only",
            attribute_details(truncated, &["Amount"]),
            truncated,
        ),
        (
            "an object type this domain deleted",
            "Deleted_Type",
            attribute_details("Dropped", &["Amount"]),
            "Dropped",
        ),
    ];

    // The last row's type really was defined once and then removed, so the refusal is about
    // what the domain holds NOW rather than about a name never seen.
    let (status, body) = emulator
        .send(
            "PUT",
            &format!("/domains/{DOMAIN}/object-types/Dropped"),
            Some(json!({ "Description": "a type about to be dropped" })),
        )
        .await;
    assert_eq!(status, 200, "seeding the dropped type: got {status} {body:?}");
    let (status, body) = emulator
        .send("DELETE", &format!("/domains/{DOMAIN}/object-types/Dropped"), None)
        .await;
    assert_eq!(status, 200, "dropping that type: got {status} {body:?}");

    for (case, name, details, object_type) in rows {
        let (status, body) = create(
            &emulator,
            DOMAIN,
            name,
            json!({ "AttributeDetails": details, "Statistic": "SUM" }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body)),
            (400, "BadRequestException"),
            "case {case:?}: expected 400 BadRequestException, got {status} {body:?}"
        );
        assert_eq!(
            error_message(&body),
            invalid_object_type(object_type),
            "case {case:?}: expected the service's wording, got {:?}",
            error_message(&body)
        );

        // A refused create stores nothing: the name it was attempted under is still free.
        let (status, read) = get(&emulator, DOMAIN, name).await;
        assert_eq!(
            (status, error_code(&read)),
            (404, "ResourceNotFoundException"),
            "case {case:?}: a refused create must store no definition, got {status} {read:?}"
        );
    }
}

/// RULE, success half: an `AttributeDetails` whose object types the domain defines is
/// admitted and reads back as it was stored — whether the type was defined by a
/// `PutProfileObjectType` call or is the standard profile model every domain holds.
#[tokio::test]
async fn a_definition_over_a_defined_object_type_is_stored_and_read_back() {
    let emulator = Emulator::new();
    create_domain_with_object_type(&emulator, DOMAIN, 30).await;

    // (case, the definition's name, the AttributeDetails)
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "the object type the domain defined",
            "Defined_Type",
            attribute_details(OBJECT_TYPE, &["Amount"]),
        ),
        (
            "two elements of the same defined type",
            "Two_Elements",
            attribute_details(OBJECT_TYPE, &["Amount", "Discount"]),
        ),
        (
            "the standard profile model every domain holds",
            "Standard_Profile",
            attribute_details(STANDARD_PROFILE_TYPE, &["AccountNumber"]),
        ),
        (
            "an attribute item qualified with the defined type",
            "Qualified_Item",
            json!({
                "Attributes": [{ "Name": format!("{OBJECT_TYPE}.Amount") }],
                "Expression": format!("{{{OBJECT_TYPE}.Amount}}"),
            }),
        ),
        (
            "an expression that qualifies nothing, so it names no object type",
            "Unqualified_Expression",
            json!({ "Attributes": [{ "Name": "Amount" }], "Expression": "{Amount}" }),
        ),
    ];

    for (case, name, details) in rows {
        let request = json!({ "AttributeDetails": details, "Statistic": "COUNT" });
        let (status, created) = create(&emulator, DOMAIN, name, request.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 for {request:?}, got {status} {created:?}"
        );
        let (status, read) = get(&emulator, DOMAIN, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the definition to be stored, got {status} {read:?}"
        );
        assert_eq!(
            read.get("AttributeDetails"),
            request.get("AttributeDetails"),
            "case {case:?}: expected the read to report the AttributeDetails the create \
             stored, got {:?}",
            read.get("AttributeDetails")
        );
    }
}

/// RULE: the object types are resolved against the DOMAIN the request addresses, so a type
/// defined in one domain only is not a type another domain's definition may name.
#[tokio::test]
async fn an_object_type_is_resolved_in_the_domain_the_request_addresses() {
    let emulator = Emulator::new();
    create_domain_with_object_type(&emulator, "holder", 30).await;
    emulator.create_domain("bystander", 30).await;

    // (case, the domain addressed, whether it defines the type)
    for (case, domain, defines) in [
        ("the domain that defines the type", "holder", true),
        ("a domain that does not", "bystander", false),
    ] {
        let (status, body) = create(
            &emulator,
            domain,
            "Order_Total",
            json!({
                "AttributeDetails": attribute_details(OBJECT_TYPE, &["Amount"]),
                "Statistic": "SUM",
            }),
        )
        .await;
        if defines {
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200, got {status} {body:?}"
            );
        } else {
            assert_eq!(
                (status, error_code(&body), error_message(&body).to_string()),
                (
                    400,
                    "BadRequestException",
                    invalid_object_type(OBJECT_TYPE)
                ),
                "case {case:?}: expected the undefined-type refusal, got {status} {body:?}"
            );
        }
    }
}
