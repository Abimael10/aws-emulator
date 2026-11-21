// SPDX-License-Identifier: CC0-1.0

//! Cloud Control API over the parameters AWS Systems Manager holds.
//!
//! The rule every case here holds the service to: **a resource of a type another service of
//! this emulator owns is resolved, enumerated, provisioned, patched and removed in THAT
//! service's own state, whichever API created it — "you can use this action to return
//! information about an existing resource in your account and Amazon Web Services Region,
//! whether those resources were provisioned using Cloud Control API" (`GetResource`) — and
//! it answers to the identifier the type's schema declares.** For `AWS::SSM::Parameter` that
//! identifier is the parameter's own name (`primaryIdentifier` `/properties/Name`), so a
//! parameter `ssm:PutParameter` wrote is one `GetResource` reports under that name, and one
//! `ssm:DeleteParameter` removed is one it reports as not found.
//!
//! The rows differ along what the rule generalises over: which API the parameter came into
//! being through, the form of its name (top level, hierarchical, and the two references a
//! top-level name is addressed by), which type and tier it is of, and which properties it
//! carries.
//!
//! `sns_topic_read_through`, `sqs_queue_read_through` and `native_resource_resolution` hold
//! the same rule for the `AWS::SNS::*`, `AWS::SQS::*` and `AWS::Logs::*` types, so what is
//! asserted across the suites is where a resource LIVES rather than anything about one
//! owning service.
//!
//! The last two cases here hold the service to a second rule, about WHICH properties such a
//! model carries: **a resource model this service reports is serialised from the resource
//! type's schema without the properties that schema declares `writeOnlyProperties`, so a
//! property a caller only ever SETS is in no `ResourceDescription.Properties` and no
//! `ProgressEvent.ResourceModel`** — it takes effect on the resource, and the owning
//! service's own reads are what report it.

use serde_json::{json, Value};

use super::emulator::{
    descriptions, properties_of, text, Emulator, SIGNING_REGION, STATUS_FAILED,
};
use super::refusal_wording::{assert_handler_not_found, generic_not_found};

/// The type whose resources live in the Systems Manager service's own store.
const PARAMETER: &str = "AWS::SSM::Parameter";

/// The properties of this type a caller SETS and no read of the resource reports.
///
/// Catalog source: the `writeOnlyProperties` of the `AWS::SSM::Parameter` resource provider
/// schema, with the `/properties/` prefix of each pointer dropped. The emulator's own
/// catalogue of that list is pinned to this one below, so a case here holds the service to
/// the schema rather than to the catalogue.
const WRITE_ONLY: [&str; 4] = ["AllowedPattern", "Description", "Policies", "Tier"];

/// The properties of this type every model reported for a parameter carries: its identity,
/// the two the schema declares required, and the data type the owning service resolved.
///
/// Catalog source: the same schema's `properties`, less the write-only ones above, less the
/// optional `Tags` a parameter need not carry.
const READABLE: [&str; 5] = ["Name", "Arn", "Type", "Value", "DataType"];

/// The ARN of the parameter called `name`, built by the OWNING service's own ARN builder so
/// a case pins the ARN Cloud Control API reported to the one that service holds rather than
/// to a string this suite spells.
fn parameter_arn(name: &str) -> String {
    service_com_amazonaws_ssm_amazonssm::state::parameter_arn(SIGNING_REGION, name)
}

/// The error code Systems Manager answers for a parameter it does not hold
/// (`ParameterNotFound`).
const PARAMETER_NOT_FOUND: &str = "ParameterNotFound";

/// The identifiers `ListResources` reports for the parameters, walking every page.
async fn inventory(emulator: &Emulator) -> Vec<String> {
    let listed = emulator.list(PARAMETER, None, None).await;
    descriptions(&listed)
        .iter()
        .map(|description| text(description, "Identifier"))
        .collect()
}

/// The `Parameter` the owning service's own read reports for `name`, required to be
/// answered.
async fn served(emulator: &Emulator, case: &str, name: &str) -> Value {
    let (status, body) = emulator.ssm("GetParameter", json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ssm:GetParameter expected HTTP 200, got {status} with body {body:?}"
    );
    body["Parameter"].clone()
}

/// The resource model `GetResource` reports for `reference`, required to be answered, with
/// the identifier the resource answers to.
async fn resolved(emulator: &Emulator, case: &str, reference: &str) -> (String, Value) {
    let (status, body) = emulator.get(PARAMETER, reference).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetResource expected HTTP 200, got {status} with body {body:?}"
    );
    let description = &body["ResourceDescription"];
    (
        text(description, "Identifier"),
        properties_of(description),
    )
}

/// One row of the native table: the case, the parameter's name, the `PutParameter` request
/// the OWNING service's own API is called with, and the resource model Cloud Control API
/// must then report for it.
type NativeRow<'a> = (&'a str, &'a str, Value, Value);

/// The parameters the native table writes, as `(name, PutParameter request, model)` rows.
///
/// The rows differ along the form of the name, the type of parameter, the tier and the
/// properties carried: a tag set, a description, a data type.
fn native_rows() -> [NativeRow<'static>; 4] {
    [
        (
            "a top-level String parameter with tags",
            "native-original",
            json!({
                "Name": "native-original",
                "Value": "native-original-value",
                "Type": "String",
                "Tags": [{ "Key": "owner", "Value": "platform" }],
            }),
            json!({
                "Name": "native-original",
                "Arn": parameter_arn("native-original"),
                "Type": "String",
                "Value": "native-original-value",
                "DataType": "text",
                "Tags": { "owner": "platform" },
            }),
        ),
        (
            "a hierarchical parameter with a description",
            "/app/db/url",
            json!({
                "Name": "/app/db/url",
                "Value": "postgres://db.internal:5432",
                "Type": "String",
                "Description": "where the database listens",
            }),
            // Neither the description the put carried nor the tier the owning service
            // resolved is in the model: both are write-only properties of this type.
            json!({
                "Name": "/app/db/url",
                "Arn": parameter_arn("/app/db/url"),
                "Type": "String",
                "Value": "postgres://db.internal:5432",
                "DataType": "text",
            }),
        ),
        (
            "a StringList parameter",
            "/app/db/hosts",
            json!({
                "Name": "/app/db/hosts",
                "Value": "one.internal,two.internal,three.internal",
                "Type": "StringList",
            }),
            json!({
                "Name": "/app/db/hosts",
                "Arn": parameter_arn("/app/db/hosts"),
                "Type": "StringList",
                "Value": "one.internal,two.internal,three.internal",
                "DataType": "text",
            }),
        ),
        (
            "an advanced-tier parameter whose data type is named",
            "advanced-image",
            json!({
                "Name": "advanced-image",
                "Value": "ami-0123456789abcdef0",
                "Type": "String",
                "Tier": "Advanced",
                "DataType": "aws:ec2:image",
            }),
            json!({
                "Name": "advanced-image",
                "Arn": parameter_arn("advanced-image"),
                "Type": "String",
                "Value": "ami-0123456789abcdef0",
                "DataType": "aws:ec2:image",
            }),
        ),
    ]
}

#[tokio::test]
async fn a_parameter_systems_manager_holds_is_resolved_and_listed_through_cloud_control() {
    let emulator = Emulator::with_ssm();
    for (case, name, put, expected) in native_rows() {
        // Created through the OWNING service's own API: nothing of this parameter is ever
        // named to Cloud Control API before the reads below.
        emulator.ssm_ok("PutParameter", put).await;
        let parameter = served(&emulator, case, name).await;
        assert_eq!(
            parameter["Name"],
            json!(name),
            "case {case:?}: the owning service must hold the parameter under its own name, \
             got {parameter:?}"
        );

        // Cloud Control API resolves the very parameter that service holds, whichever
        // reference addresses it: a top-level name is the one parameter whether or not the
        // reference carries the hierarchy separator.
        let mut references = vec![("the name as written", name.to_string())];
        if !name.starts_with('/') {
            references.push(("the name with a leading separator", format!("/{name}")));
        }
        for (reference, addressed) in references {
            let (identifier, model) = resolved(&emulator, case, &addressed).await;
            assert_eq!(
                identifier, name,
                "case {case:?} ({reference}): expected the parameter's own name, got \
                 {identifier:?}"
            );
            assert_eq!(
                model, expected,
                "case {case:?} ({reference}): expected {expected:?}, got {model:?}"
            );
            // The ARN in the model is the one the OWNING service reports for the very same
            // parameter, so the two planes name one resource.
            assert_eq!(
                model["Arn"], parameter["ARN"],
                "case {case:?} ({reference}): expected the ARN the owning service reports \
                 ({:?}), got {:?}",
                parameter["ARN"], model["Arn"]
            );
        }
        assert!(
            inventory(&emulator).await.contains(&name.to_string()),
            "case {case:?}: the parameter must be in the type's inventory"
        );
    }

    // The whole inventory is the owning store's own, in its own order — and it pages, one
    // resource at a time, over exactly those parameters.
    let expected: Vec<String> = vec![
        "advanced-image".to_string(),
        "/app/db/hosts".to_string(),
        "/app/db/url".to_string(),
        "native-original".to_string(),
    ];
    for (case, page_size) in [("one page", None), ("one resource per page", Some(1))] {
        let walked = emulator.walk(PARAMETER, page_size).await;
        assert_eq!(
            walked, expected,
            "case {case:?}: expected the parameters the owning service holds, got {walked:?}"
        );
    }
}

#[tokio::test]
async fn a_parameter_deleted_in_either_plane_stops_resolving_in_both() {
    let emulator = Emulator::with_ssm();
    // The rows differ along WHICH API removed the parameter: the owning service's own, and
    // Cloud Control API. Either way the parameter is gone from the one store it lived in,
    // so both planes report it as absent.
    let rows = [
        ("removed through the owning service's own API", "doomed-native", false),
        ("removed through Cloud Control API", "/doomed/by-cloud-control", true),
    ];
    for (case, name, through_cloud_control) in rows {
        emulator
            .ssm_ok(
                "PutParameter",
                json!({ "Name": name, "Value": "v", "Type": "String" }),
            )
            .await;
        let (identifier, _) = resolved(&emulator, case, name).await;
        assert_eq!(
            identifier, name,
            "case {case:?}: the live parameter must resolve under its own name"
        );

        if through_cloud_control {
            emulator.delete_and_settle(PARAMETER, name).await;
        } else {
            emulator
                .ssm_ok("DeleteParameter", json!({ "Name": name }))
                .await;
        }

        // The owning service no longer holds it, in its own terms.
        let (status, body) = emulator.ssm("GetParameter", json!({ "Name": name })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: ssm:GetParameter after the delete expected HTTP 400, got \
             {status} with body {body:?}"
        );
        assert!(
            super::emulator::code(&body).contains(PARAMETER_NOT_FOUND),
            "case {case:?}: expected {PARAMETER_NOT_FOUND}, got {:?}",
            super::emulator::code(&body)
        );

        // And Cloud Control API reports the handler's not-found verdict for it.
        let (status, body) = emulator.get(PARAMETER, name).await;
        assert_eq!(
            status, 400,
            "case {case:?}: GetResource after the delete expected HTTP 400, got {status} \
             with body {body:?}"
        );
        assert_handler_not_found(
            &super::emulator::message(&body),
            PARAMETER,
            &generic_not_found(PARAMETER, name),
            case,
        );
        assert!(
            !inventory(&emulator).await.contains(&name.to_string()),
            "case {case:?}: the deleted parameter must be listed nowhere"
        );
    }
}

/// One row of the create table: the case, the parameter's name, the desired state the
/// create carries, and what the owning service must then report for it — its type, its
/// value and its tags.
type CreateRow<'a> = (&'a str, &'a str, Value, Vec<(&'a str, &'a str)>);

#[tokio::test]
async fn a_parameter_created_through_cloud_control_is_the_parameter_systems_manager_holds() {
    let emulator = Emulator::with_ssm();
    // The rows differ along the form of the name, the type and tier asked for, and the
    // properties the desired state carries.
    let rows: [CreateRow; 3] = [
        (
            "a top-level String parameter",
            "provisioned",
            json!({ "Name": "provisioned", "Type": "String", "Value": "v1" }),
            vec![],
        ),
        (
            "a hierarchical parameter with a description and tags",
            "/fleet/config/region",
            json!({
                "Name": "/fleet/config/region",
                "Type": "String",
                "Value": "us-east-1",
                "Description": "the Region the fleet runs in",
                "Tags": { "team": "platform", "tier": "gold" },
            }),
            vec![("team", "platform"), ("tier", "gold")],
        ),
        (
            "an advanced StringList parameter",
            "/fleet/config/hosts",
            json!({
                "Name": "/fleet/config/hosts",
                "Type": "StringList",
                "Value": "one,two",
                "Tier": "Advanced",
            }),
            vec![],
        ),
    ];

    for (case, name, desired, tags) in rows {
        let identifier = emulator.provision(PARAMETER, &desired).await;
        assert_eq!(
            identifier, name,
            "case {case:?}: expected the parameter's own name, got {identifier:?}"
        );

        // The service that OWNS the type holds the parameter and answers for it in its own
        // terms, as the FIRST version of it.
        let parameter = served(&emulator, case, name).await;
        for (member, expected) in [
            ("Name", json!(name)),
            ("Type", desired["Type"].clone()),
            ("Value", desired["Value"].clone()),
            ("Version", json!(1)),
        ] {
            assert_eq!(
                parameter[member], expected,
                "case {case:?} ({member}): expected {expected:?}, got {:?}",
                parameter[member]
            );
        }

        // The tags the create carried are tags that service holds.
        let listed = emulator
            .ssm_ok(
                "ListTagsForResource",
                json!({ "ResourceType": "Parameter", "ResourceId": name }),
            )
            .await;
        let held: Vec<(String, String)> = listed["TagList"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|tag| (text(tag, "Key"), text(tag, "Value")))
            .collect();
        let expected_tags: Vec<(String, String)> = tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        assert_eq!(
            held, expected_tags,
            "case {case:?}: expected the owning service to hold {expected_tags:?}, got \
             {held:?}"
        );

        // And Cloud Control API reads back the model that service holds: the desired state
        // without the properties the schema declares write-only, plus the data type the
        // owning service resolved for it.
        let (_, model) = resolved(&emulator, case, name).await;
        for (property, expected) in desired.as_object().expect("an object of properties") {
            if WRITE_ONLY.contains(&property.as_str()) {
                assert!(
                    model.get(property).is_none(),
                    "case {case:?} ({property}): a write-only property must be reported by \
                     nothing, got {model:?}"
                );
                continue;
            }
            assert_eq!(
                &model[property], expected,
                "case {case:?} ({property}): expected {expected:?}, got {:?}",
                model[property]
            );
        }
        assert_eq!(
            model["DataType"],
            json!("text"),
            "case {case:?}: expected the owning service's own default data type, got \
             {model:?}"
        );
        assert!(
            model.get("Tier").is_none(),
            "case {case:?}: the tier is write-only, so no reported model carries it, got \
             {model:?}"
        );
        assert_eq!(
            model["Arn"],
            json!(parameter_arn(name)),
            "case {case:?}: expected the ARN the owning service builds for the parameter, \
             got {model:?}"
        );
    }
}

#[tokio::test]
async fn a_patch_through_cloud_control_is_a_new_version_of_the_parameter_systems_manager_holds() {
    let emulator = Emulator::with_ssm();
    let name = "patched";
    let identifier = emulator
        .provision(
            PARAMETER,
            &json!({ "Name": name, "Type": "String", "Value": "before" }),
        )
        .await;

    // Each patch of a property this path applies is a WRITE to the parameter the owning
    // service holds, which is a new version of it: "create or update a parameter in
    // Parameter Store". The rows differ along the property patched and the version the
    // write leaves behind.
    let rows = [
        (
            "the value",
            json!([{ "op": "replace", "path": "/Value", "value": "after" }]),
            "after",
            2,
        ),
        (
            "the value again",
            json!([{ "op": "replace", "path": "/Value", "value": "after-again" }]),
            "after-again",
            3,
        ),
    ];
    for (case, patch, expected_value, expected_version) in rows {
        emulator.update_and_settle(PARAMETER, &identifier, &patch).await;
        let parameter = served(&emulator, case, name).await;
        assert_eq!(
            parameter["Value"],
            json!(expected_value),
            "case {case:?}: expected the owning service to hold {expected_value:?}, got \
             {parameter:?}"
        );
        assert_eq!(
            parameter["Version"],
            json!(expected_version),
            "case {case:?}: expected version {expected_version}, got {parameter:?}"
        );
    }

    // A patch asking for a property of the type no handler here applies is not applied at
    // all: the request reaches the owning handler's verdict, and the parameter keeps the
    // model it had.
    let rows = [
        ("the allowed pattern", "AllowedPattern", json!("^[0-9]+$")),
        (
            "the parameter policies",
            "Policies",
            json!("[{\"Type\":\"Expiration\",\"Version\":\"1.0\"}]"),
        ),
    ];
    for (case, property, value) in rows {
        let (status, accepted) = emulator
            .update(
                PARAMETER,
                &identifier,
                &json!([{ "op": "add", "path": format!("/{property}"), "value": value }]),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the request to be accepted, got {status} with body \
             {accepted:?}"
        );
        let event = emulator
            .terminal(&text(&super::emulator::progress(&accepted), "RequestToken"))
            .await;
        assert_eq!(
            text(&event, "OperationStatus"),
            STATUS_FAILED,
            "case {case:?}: expected the request to fail, got {event:?}"
        );
        assert_eq!(
            text(&event, "ErrorCode"),
            "InvalidRequest",
            "case {case:?}: expected the owning handler's verdict, got {event:?}"
        );
        assert_eq!(
            text(&event, "StatusMessage"),
            format!(
                "{property} is a property of {PARAMETER} this emulator's handler does not \
                 apply"
            ),
            "case {case:?}: unexpected wording, got {event:?}"
        );
        let parameter = served(&emulator, case, name).await;
        assert_eq!(
            (parameter["Value"].clone(), parameter["Version"].clone()),
            (json!("after-again"), json!(3)),
            "case {case:?}: the refused patch must leave the parameter as it was, got \
             {parameter:?}"
        );
    }
}

#[tokio::test]
async fn a_create_the_owning_service_would_refuse_fails_with_the_handlers_verdict() {
    let emulator = Emulator::with_ssm();
    // The rows differ along WHICH rule the desired state breaks: the naming rules the owning
    // service holds every parameter name to (the hierarchy's leading separator, the prefixes
    // it keeps for itself), the two properties the schema declares required, and a property
    // the type does not declare at all.
    //
    // A value outside a set the TYPE'S REGISTRY SCHEMA publishes — a parameter type or tier
    // it does not carry — is not a row here: such a model never reaches a handler at all, the
    // front door refusing the call before a request is minted
    // (`front_door_model_validation`).
    let rows: [(&str, &str, Value, String); 5] = [
        (
            "a hierarchical name with no leading separator",
            "Dev/DBServer",
            json!({ "Name": "Dev/DBServer", "Type": "String", "Value": "v" }),
            "Parameter name must be a fully qualified name.".to_string(),
        ),
        (
            "a name in the hierarchy the owning service keeps for itself",
            "/aws/reserved",
            json!({ "Name": "/aws/reserved", "Type": "String", "Value": "v" }),
            "Name '/aws/reserved' is prefixed with 'aws': a parameter name can't be \
             prefixed with aws or ssm (case-insensitive)"
                .to_string(),
        ),
        (
            "a model that names no value",
            "refused-valueless",
            json!({ "Name": "refused-valueless", "Type": "String" }),
            format!("Value is required by {PARAMETER}"),
        ),
        (
            "a model that names no type",
            "refused-typeless",
            json!({ "Name": "refused-typeless", "Value": "v" }),
            format!("Type is required by {PARAMETER}"),
        ),
        (
            "a property the type does not declare",
            "refused-stranger",
            json!({ "Name": "refused-stranger", "Type": "String", "Value": "v", "Nope": "1" }),
            format!("Nope is not a property of {PARAMETER}"),
        ),
    ];

    for (case, name, desired, expected_message) in rows {
        let (status, body) = emulator.create(PARAMETER, &desired, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to be accepted, got {status} with body \
             {body:?}"
        );
        let event = emulator
            .terminal(&text(&super::emulator::progress(&body), "RequestToken"))
            .await;
        assert_eq!(
            text(&event, "OperationStatus"),
            STATUS_FAILED,
            "case {case:?}: expected the request to fail, got {event:?}"
        );
        assert_eq!(
            text(&event, "ErrorCode"),
            "InvalidRequest",
            "case {case:?}: expected the owning handler's verdict, got {event:?}"
        );
        assert_eq!(
            text(&event, "StatusMessage"),
            expected_message,
            "case {case:?}: unexpected wording, got {event:?}"
        );
        // Nothing was written, in either plane: a refused create leaves no resource behind.
        let (status, body) = emulator.get(PARAMETER, name).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the refused create must leave no resource, got {status} with \
             body {body:?}"
        );
    }
}

/// The `ParameterMetadata` the OWNING service's own describe reports for `name`, required
/// to be answered — the read that reports the properties this type declares write-only.
async fn described(emulator: &Emulator, case: &str, name: &str) -> Value {
    let reported = emulator
        .ssm_ok(
            "DescribeParameters",
            json!({
                "ParameterFilters": [{ "Key": "Name", "Option": "Equals", "Values": [name] }],
            }),
        )
        .await;
    reported["Parameters"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|parameter| parameter["Name"] == json!(name))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: ssm:DescribeParameters must report the parameter {name:?}, got \
                 {reported:?}"
            )
        })
}

/// The resource model `ListResources` reports for the parameter `name`, required to be in
/// the type's inventory.
async fn listed(emulator: &Emulator, case: &str, name: &str) -> Value {
    let page = emulator.list(PARAMETER, None, None).await;
    let found = descriptions(&page)
        .into_iter()
        .find(|description| text(description, "Identifier") == name)
        .unwrap_or_else(|| {
            panic!("case {case:?}: ListResources must report the parameter {name:?}, got {page:?}")
        });
    properties_of(&found)
}

/// Assert that `model` carries no property the type's schema declares write-only, and every
/// property a read of a parameter reports.
fn assert_projection(model: &Value, case: &str, source: &str) {
    for property in WRITE_ONLY {
        assert!(
            model.get(property).is_none(),
            "case {case:?} ({source}, {property}): a write-only property is in no resource \
             model this service reports, got {model:?}"
        );
    }
    for property in READABLE {
        assert!(
            model.get(property).is_some(),
            "case {case:?} ({source}, {property}): a readable property must be reported, got \
             {model:?}"
        );
    }
}

#[tokio::test]
async fn a_write_only_property_is_in_no_resource_model_this_service_reports() {
    // The list the service projects a reported model through is the schema's own, so the
    // cases below hold this service to the resource type's schema rather than to a
    // catalogue of its own.
    let catalogued = service_com_amazonaws_cloudcontrol_cloudapiservice::state::write_only_properties(
        PARAMETER,
    );
    assert_eq!(
        catalogued, WRITE_ONLY,
        "expected the schema's writeOnlyProperties, got {catalogued:?}"
    );

    let emulator = Emulator::with_ssm();
    // The rows differ along WHICH of the type's write-only properties the parameter
    // carries, and how many: the tier every parameter is served in and nothing else, a
    // description beside it, a pattern beside that, and all four at once on an advanced
    // parameter. Every put is the OWNING service's own, so each of these properties did
    // take effect on the parameter it names.
    let rows: [(&str, &str, Value); 4] = [
        (
            "no write-only property beyond the tier every parameter has",
            "/write-only/bare",
            json!({ "Name": "/write-only/bare", "Type": "StringList", "Value": "one,two" }),
        ),
        (
            "a description",
            "/write-only/described",
            json!({
                "Name": "/write-only/described",
                "Type": "String",
                "Value": "abc123",
                "Description": "desc-v1",
            }),
        ),
        (
            "a description and the pattern the value is validated against",
            "/write-only/patterned",
            json!({
                "Name": "/write-only/patterned",
                "Type": "String",
                "Value": "42",
                "Description": "digits only",
                "AllowedPattern": "^[0-9]+$",
            }),
        ),
        (
            "every write-only property at once, on an advanced parameter",
            "/write-only/advanced",
            json!({
                "Name": "/write-only/advanced",
                "Type": "String",
                "Value": "abc123",
                "Description": "desc-v1",
                "AllowedPattern": "^[a-zA-Z0-9]+$",
                "Tier": "Advanced",
                "Policies": "[{\"Type\":\"Expiration\",\"Version\":\"1.0\"}]",
            }),
        ),
    ];

    for (case, name, put) in rows {
        emulator.ssm_ok("PutParameter", put.clone()).await;
        let metadata = described(&emulator, case, name).await;
        // Each write-only property the put carried is one the OWNING service reports...
        for property in WRITE_ONLY {
            let Some(sent) = put.get(property).and_then(Value::as_str) else {
                continue;
            };
            if property == "Policies" {
                // Reported as the policy list the document parsed to rather than as the
                // document, so what is pinned here is that the parameter carries policies.
                assert!(
                    metadata[property]
                        .as_array()
                        .is_some_and(|policies| !policies.is_empty()),
                    "case {case:?}: the owning service must report the policies the put \
                     carried, got {metadata:?}"
                );
                continue;
            }
            assert_eq!(
                metadata[property],
                json!(sent),
                "case {case:?} ({property}): expected the owning service to report {sent:?}, \
                 got {metadata:?}"
            );
        }
        // ...the tier included, which that service resolves for every parameter whether a
        // write names one or not.
        assert!(
            metadata["Tier"]
                .as_str()
                .is_some_and(|tier| !tier.is_empty()),
            "case {case:?}: the owning service must report the tier it resolved, got \
             {metadata:?}"
        );

        // ...and one no model THIS service reports carries, whichever read reports it.
        for (source, model) in [
            ("GetResource", resolved(&emulator, case, name).await.1),
            ("ListResources", listed(&emulator, case, name).await),
        ] {
            assert_projection(&model, case, source);
            assert_eq!(
                model["Value"], put["Value"],
                "case {case:?} ({source}): expected the value the parameter holds, got \
                 {model:?}"
            );
        }
    }
}

/// Assert that the resource model `event` reports is projected as the schema serialises it,
/// while the OWNING service reports the write-only values the request applied: the
/// `description` it set, and the tier it is served in.
async fn assert_settled_model(emulator: &Emulator, case: &str, event: &Value, description: &str) {
    let model: Value = serde_json::from_str(&text(event, "ResourceModel"))
        .unwrap_or_else(|_| panic!("case {case:?}: expected a resource model, got {event:?}"));
    assert_projection(&model, case, "ProgressEvent.ResourceModel");
    // What the model leaves out is a value that DID take effect: the owning service reports
    // the description the request applied, and the tier the resource is served in.
    let name = text(event, "Identifier");
    let metadata = described(emulator, case, &name).await;
    assert_eq!(
        metadata["Description"],
        json!(description),
        "case {case:?}: expected the owning service to report the description the request \
         applied, got {metadata:?}"
    );
    assert_eq!(
        metadata["Tier"],
        json!("Advanced"),
        "case {case:?}: expected the owning service to report the tier the create asked for, \
         got {metadata:?}"
    );
}

#[tokio::test]
async fn the_model_a_mutating_request_leaves_behind_carries_no_write_only_property() {
    let emulator = Emulator::with_ssm();
    let name = "/write-only/progress";
    // The two requests differ along WHICH one leaves the model behind: a create whose
    // desired state carries two of the type's write-only properties, and the one patch
    // operation a write-only property may be named by.
    let created = emulator
        .create_ok(
            PARAMETER,
            &json!({
                "Name": name,
                "Type": "String",
                "Value": "v1",
                "Description": "set by the create",
                "Tier": "Advanced",
            }),
            None,
        )
        .await;
    let created = emulator.settled(&text(&created, "RequestToken")).await;
    assert_settled_model(&emulator, "the create", &created, "set by the create").await;

    let patched = emulator
        .update_and_settle(
            PARAMETER,
            name,
            &json!([{ "op": "add", "path": "/Description", "value": "set by the patch" }]),
        )
        .await;
    assert_settled_model(&emulator, "the patch", &patched, "set by the patch").await;
}
