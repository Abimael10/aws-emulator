// SPDX-License-Identifier: CC0-1.0

//! WHICH attributes a created brand profile is seeded with: the service's own
//! default attribute names, each with the `attributeType` it takes and the
//! `category` it is grouped under.
//!
//! A child suite of `brand_profile_default_attributes`, which owns the seeding,
//! paging and addressing rules the template is read back through; this file owns the
//! CONTENTS of that template. Its parent's assertions walk the crate's own table, so
//! they hold for any table at all — this suite names the service's default attributes
//! independently of it, which is what tells the service's template apart from a set of
//! placeholder slots. Declared by its parent, because this crate's one test target
//! declares the suites directly under `tests/all/`.
//!
//! The rules under test, each over a SET of inputs rather than one recorded request:
//!
//! * a created brand profile carries the service's default attribute template, so
//!   every default attribute is reported under its own service name — in whatever
//!   form that name takes, one word or several words separated by spaces — by the
//!   attribute list and by the single-attribute read, each with that attribute's
//!   `attributeType` and `category`;
//! * the seed is a CLASSIFIED template: every seeded entry carries an
//!   `attributeType` the model declares and a `category` the service supports, with
//!   the member set on every one of them, for every create input;
//! * `attributesCreated` is the number of attributes the new profile then holds, and
//!   the profile holds at least the whole default template;
//! * a default attribute is addressable by its own service name for the whole of its
//!   lifecycle: readable right after the create, deleted by that name, then refused
//!   to the reader with `ResourceNotFoundException` worded over the attribute
//!   resource type, and creatable again under the same name with a type and value
//!   the caller supplies.

use serde_json::{json, Value};

use service_com_amazonaws_endusermessaging_endusermessaging::state::{
    is_supported_attribute_category, BRAND_PROFILE_ATTRIBUTE_RESOURCE_TYPE,
};

use super::{
    accepted_names, is_modelled_attribute_name, requested_names, Emulator, ATTRIBUTE_TYPES,
};

/// The service's default attribute template: the `attributeName` of a default
/// attribute every created brand profile carries, the `attributeType` it takes and
/// the `category` it is grouped under.
///
/// Source: the service's own `ListBrandProfileAttributes` answer for a freshly
/// created brand profile. The model does not enumerate the template —
/// `smithy_query(query="shape", shape="BrandProfileAttributeSummary")` gives the
/// members each entry carries (`attributeName`, `attributeType`, `category`) and the
/// constraints on them, and nothing in it lists the set — so the names are written
/// out here rather than read from the crate's table, and the rows cover both forms
/// `BrandProfileAttributeName`'s pattern admits for a default name (one word and
/// several words separated by spaces) and every category the template classifies its
/// attributes under.
const SERVICE_DEFAULT_TEMPLATE: &[(&str, &str, &str)] = &[
    ("Address Line 2", "TEXT", "BUSINESS_ADDRESS"),
    ("Business Type", "TEXT", "BUSINESS_IDENTITY"),
    ("Business Vertical", "TEXT", "BUSINESS_IDENTITY"),
    ("City", "TEXT", "BUSINESS_ADDRESS"),
    ("Company Address", "TEXT", "BUSINESS_ADDRESS"),
    ("Company Name", "TEXT", "BUSINESS_IDENTITY"),
    ("Contact Email", "TEXT", "BUSINESS_CONTACT"),
];

/// The `category` of one listed summary or one read attribute, which every seeded
/// attribute carries.
fn category(case: &str, body: &Value) -> String {
    body["category"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: category is not set in {body:?}"))
        .to_string()
}

/// The `attributeType` of one listed summary or one read attribute.
fn attribute_type(case: &str, body: &Value) -> String {
    body["attributeType"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: attributeType is not set in {body:?}"))
        .to_string()
}

#[tokio::test]
async fn a_created_profile_carries_the_services_own_default_attributes() {
    let em = Emulator::new();
    let created = em.create_named("service-default-template").await;
    let id = created.string("brandProfileId");
    let listed = em.all_attributes("service template", &id, None).await;

    for (name, expected_type, expected_category) in SERVICE_DEFAULT_TEMPLATE {
        let case = format!("attributeName={name:?}");
        let summary = listed
            .iter()
            .find(|item| item["attributeName"] == json!(name))
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: the seeded template must hold it, got {:?}",
                    requested_names(&listed)
                )
            });
        assert_eq!(
            attribute_type(&case, summary),
            *expected_type,
            "case {case}: the list reports the service's attributeType, got {summary:?}"
        );
        assert_eq!(
            category(&case, summary),
            *expected_category,
            "case {case}: the list reports the service's category, got {summary:?}"
        );

        // The same default attribute, addressed by that very name through the
        // single-attribute read: one stored record, one rendering of it.
        let read = em.get_attribute(&id, name).await;
        assert_eq!(
            read.status, 200,
            "case {case}: a default attribute reads back by its own name, got {} / {:?}",
            read.status, read.body
        );
        assert_eq!(
            attribute_type(&case, &read.body),
            *expected_type,
            "case {case}: the read reports the service's attributeType, got {:?}",
            read.body
        );
        assert_eq!(
            category(&case, &read.body),
            *expected_category,
            "case {case}: the read reports the service's category, got {:?}",
            read.body
        );
    }
}

#[tokio::test]
async fn every_seeded_attribute_is_named_typed_and_classified() {
    let em = Emulator::new();

    // The rows are the classes of create input: every brand-profile name the
    // modelled pattern accepts, with deletion protection unset and set — the seed is
    // the service's whatever the request asked for.
    for name in accepted_names() {
        for protection in [None, Some(true)] {
            let case = format!("brandProfileName={name:?} deletionProtectionEnabled={protection:?}");
            let mut body = json!({ "brandProfileName": name });
            if let Some(enabled) = protection {
                body["deletionProtectionEnabled"] = json!(enabled);
            }
            let created = em.create(body).await;
            assert_eq!(
                created.status, 200,
                "case {case}: the create must be served, got {} / {:?}",
                created.status, created.body
            );
            let id = created.string("brandProfileId");
            let listed = em.all_attributes(&case, &id, None).await;

            for item in &listed {
                let row = format!("{case} attribute={item:?}");
                let attribute_name = item["attributeName"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {row}: attributeName is not a string"));
                assert!(
                    is_modelled_attribute_name(attribute_name),
                    "case {row}: a seeded name is within the modelled BrandProfileAttributeName"
                );
                let seeded_type = attribute_type(&row, item);
                assert!(
                    ATTRIBUTE_TYPES.contains(&seeded_type.as_str()),
                    "case {row}: a seeded attributeType is one the model declares, \
                     got {seeded_type:?}"
                );
                let seeded_category = category(&row, item);
                assert!(
                    is_supported_attribute_category(&seeded_category),
                    "case {row}: a seeded category is one the service supports, \
                     got {seeded_category:?}"
                );
            }

            assert_eq!(
                created.number("attributesCreated"),
                listed.len() as f64,
                "case {case}: attributesCreated counts the attributes the profile holds, \
                 got {:?} for {:?}",
                created.body,
                requested_names(&listed)
            );
            assert!(
                listed.len() >= SERVICE_DEFAULT_TEMPLATE.len(),
                "case {case}: the profile holds at least the service's default template, \
                 got {:?}",
                requested_names(&listed)
            );
        }
    }
}

#[tokio::test]
async fn a_default_attribute_is_addressed_by_its_service_name_for_its_whole_lifecycle() {
    let em = Emulator::new();
    let created = em.create_named("default-name-lifecycle").await;
    let id = created.string("brandProfileId");

    // Every default attribute of the service's template, so the lifecycle is driven
    // by the service's own names rather than by one of them.
    for (name, _, _) in SERVICE_DEFAULT_TEMPLATE {
        let case = format!("attributeName={name:?}");
        assert_eq!(
            em.get_attribute(&id, name).await.status,
            200,
            "case {case}: the seeded attribute reads back before the delete"
        );

        let deleted = em.delete_attribute(&id, name).await;
        assert_eq!(
            deleted.status, 200,
            "case {case}: a seeded attribute is deleted by its own name, got {} / {:?}",
            deleted.status, deleted.body
        );

        let refused = em.get_attribute(&id, name).await;
        assert_eq!(
            refused.status, 404,
            "case {case}: the deleted attribute is gone, got {} / {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case}: unexpected error code, got {:?}",
            refused.error
        );
        // `BrandProfileAttributeName` is `@sensitive`, so the refusal states the
        // resource type in its text and the name in the member modelled for it.
        let expected =
            format!("Resource not found - ResourceType=\"{BRAND_PROFILE_ATTRIBUTE_RESOURCE_TYPE}\"");
        assert_eq!(
            refused.string("message"),
            expected,
            "case {case}: expected message {expected:?}, got {:?}",
            refused.body
        );

        // The name is the service's, so it is free for a create again — with the
        // type and value the caller supplies this time.
        let recreated = em
            .create_attributes(
                &id,
                vec![json!({
                    "attributeName": name,
                    "attributeType": "TEXT",
                    "attributeValue": format!("a value for {name}"),
                })],
            )
            .await;
        assert_eq!(
            recreated.status, 200,
            "case {case}: the name is free again, got {} / {:?}",
            recreated.status, recreated.body
        );
        let read = em.get_attribute(&id, name).await;
        assert_eq!(
            read.status, 200,
            "case {case}: the re-created attribute reads back, got {} / {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.string("attributeValue"),
            format!("a value for {name}"),
            "case {case}: the stored value is the one supplied, got {:?}",
            read.body
        );
    }
}
