// SPDX-License-Identifier: CC0-1.0

//! How `UpdateResource` words the refusal of a patch operation the resource model will not
//! take, and which operation a property that may only be SET admits.
//!
//! Cloud Control API funnels every `PatchDocument` violation into ONE error code, so the
//! message is the only thing that tells a client which rule it broke. The two rules each
//! suite here holds the service to:
//!
//! * **a patch operation whose pointer resolves to nothing in the resource model it is
//!   applied to is refused in the patch reader's own terms — the operation RFC 6902 names,
//!   uppercased, and the path the request provided behind the reader's root separator — so
//!   an absent path is distinguishable from every other reason a document would not
//!   apply**, and
//! * **a property the resource type's schema declares write-only may only be `add`ed: any
//!   other operation naming one is refused for naming it, whatever the stored model
//!   carries, because the type's read handler reports nothing there to update in place.**
//!
//! The rows differ along what the rules generalise over: the patch operation, the depth and
//! form of the pointer, which of the document's operations breaks the rule, which property
//! is named, and which resource type owns the resource.

use serde_json::{json, Value};

use super::emulator::{code, message, text, Emulator, STATUS_SUCCESS};

/// A queue type, created with nothing but its name so the model carries few properties.
const QUEUE: &str = "AWS::SQS::Queue";

/// A table type, whose model carries nested objects and a list to point into.
const TABLE: &str = "AWS::DynamoDB::Table";

/// A log group type, whose schema's `RetentionInDays` is a property the owning service
/// reports only once something has set it.
const GROUP: &str = "AWS::Logs::LogGroup";

/// A parameter type, the one whose schema declares write-only properties.
const PARAMETER: &str = "AWS::SSM::Parameter";

/// One row of the absent-pointer table: what it is about, the type that owns the resource,
/// the patch document, and the operation and path the refusal must name.
struct NoSuchPath {
    /// What this row is about.
    case: &'static str,
    /// The resource type the patch is applied to.
    type_name: &'static str,
    /// The patch document the request carries.
    patch: Value,
    /// The RFC 6902 operation name the refusal names, as the reader spells it.
    operation: &'static str,
    /// The path the refusal reports, as the request provided it.
    path: &'static str,
}

#[tokio::test]
async fn a_patch_operation_whose_pointer_resolves_to_nothing_is_refused_as_no_such_path() {
    let emulator = Emulator::new();
    let queue = emulator
        .provision(QUEUE, &json!({ "QueueName": "no-such-path-queue" }))
        .await;
    let table = emulator
        .provision(
            TABLE,
            &json!({
                "TableName": "no-such-path-table",
                "Tags": [],
                "PointInTimeRecoverySpecification": { "PointInTimeRecoveryEnabled": true },
            }),
        )
        .await;

    let rows = [
        NoSuchPath {
            case: "replacing a top-level property the model does not carry",
            type_name: QUEUE,
            patch: json!([{ "op": "replace", "path": "/NoSuchProperty", "value": 1 }]),
            operation: "REPLACE",
            path: "/NoSuchProperty",
        },
        NoSuchPath {
            case: "replacing a member of an object the model does not carry",
            type_name: QUEUE,
            patch: json!([
                { "op": "replace", "path": "/RedrivePolicy/maxReceiveCount", "value": 4 },
            ]),
            operation: "REPLACE",
            path: "/RedrivePolicy/maxReceiveCount",
        },
        NoSuchPath {
            case: "removing a property the model does not carry",
            type_name: QUEUE,
            patch: json!([{ "op": "remove", "path": "/NoSuchProperty" }]),
            operation: "REMOVE",
            path: "/NoSuchProperty",
        },
        NoSuchPath {
            case: "adding into an object the model does not carry",
            type_name: TABLE,
            patch: json!([{ "op": "add", "path": "/Missing/Deeper", "value": 1 }]),
            operation: "ADD",
            path: "/Missing/Deeper",
        },
        NoSuchPath {
            case: "moving from a path the model does not carry",
            type_name: TABLE,
            patch: json!([
                { "op": "move", "from": "/Missing", "path": "/SSESpecification" },
            ]),
            operation: "MOVE",
            path: "/Missing",
        },
        NoSuchPath {
            case: "copying from a path the model does not carry",
            type_name: TABLE,
            patch: json!([
                { "op": "copy", "from": "/Missing", "path": "/SSESpecification" },
            ]),
            operation: "COPY",
            path: "/Missing",
        },
        NoSuchPath {
            case: "an index past the end of a list the model does carry",
            type_name: TABLE,
            patch: json!([{ "op": "replace", "path": "/Tags/3", "value": 1 }]),
            operation: "REPLACE",
            path: "/Tags/3",
        },
        NoSuchPath {
            case: "a document whose SECOND operation is the one that finds nothing",
            type_name: TABLE,
            patch: json!([
                { "op": "add", "path": "/TableClass", "value": "STANDARD" },
                { "op": "replace", "path": "/Missing", "value": 1 },
            ]),
            operation: "REPLACE",
            path: "/Missing",
        },
    ];

    for row in rows {
        let identifier = if row.type_name == QUEUE { &queue } else { &table };
        let before = emulator.properties(row.type_name, identifier).await;
        let (status, body) = emulator
            .update(row.type_name, identifier, &row.patch, None)
            .await;
        let case = row.case;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body:?}"
        );
        assert!(
            code(&body).contains("ValidationException"),
            "case {case:?}: expected ValidationException, got {:?}",
            code(&body)
        );
        assert_eq!(
            message(&body),
            format!(
                "[{} Operation] noSuchPath in source, path provided : /{}",
                row.operation, row.path
            ),
            "case {case:?}: unexpected wording, got {:?}",
            message(&body)
        );
        let after = emulator.properties(row.type_name, identifier).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused patch must change nothing, got {after:?}"
        );
    }

    // The neighbouring rule: an operation whose pointer DID resolve is not an absent path,
    // so it keeps the wording for a document that would not apply — which is what makes the
    // absent-path refusal above tell a client something.
    let (status, body) = emulator
        .update(
            TABLE,
            &table,
            &json!([{ "op": "test", "path": "/TableName", "value": "other" }]),
            None,
        )
        .await;
    assert_eq!(
        status, 400,
        "a test that does not hold: expected HTTP 400, got {status} with body {body:?}"
    );
    assert_eq!(
        message(&body),
        "Invalid PatchDocument: the test operation at path '/TableName' could not be applied \
         to the resource model",
        "a test that does not hold: unexpected wording, got {:?}",
        message(&body)
    );
}

#[tokio::test]
async fn a_property_the_owning_service_reports_once_set_is_added_before_it_can_be_replaced() {
    let emulator = Emulator::with_cloudwatch_logs();
    let group = "no-such-path-group";
    let identifier = emulator
        .provision(GROUP, &json!({ "LogGroupName": group }))
        .await;

    // The owning service reports no retention for a group under none, so the model carries
    // no such property to replace.
    let model = emulator.properties(GROUP, &identifier).await;
    assert_eq!(
        model.get("RetentionInDays"),
        None,
        "a group under no retention must carry no RetentionInDays, got {model:?}"
    );

    let replace = json!([{ "op": "replace", "path": "/RetentionInDays", "value": 7 }]);
    let (status, body) = emulator.update(GROUP, &identifier, &replace, None).await;
    assert_eq!(
        status, 400,
        "replacing an absent retention: expected HTTP 400, got {status} with body {body:?}"
    );
    assert_eq!(
        message(&body),
        "[REPLACE Operation] noSuchPath in source, path provided : //RetentionInDays",
        "replacing an absent retention: unexpected wording, got {:?}",
        message(&body)
    );
    let model = emulator.properties(GROUP, &identifier).await;
    assert_eq!(
        model.get("RetentionInDays"),
        None,
        "a refused replace must not create the property, got {model:?}"
    );

    // `add` is the operation that SETS a property the model does not carry, and once it has
    // been set the identical replace applies — read back through `GetResource`, which is
    // where a client reads the model it patches.
    // Catalog source: the `retentionInDays` values `logs:PutRetentionPolicy` publishes,
    // of which 7 and 14 are two.
    for (case, patch, expected) in [
        (
            "adding the retention",
            json!([{ "op": "add", "path": "/RetentionInDays", "value": 7 }]),
            7,
        ),
        (
            "replacing the retention it now carries",
            json!([{ "op": "replace", "path": "/RetentionInDays", "value": 14 }]),
            14,
        ),
    ] {
        let settled = emulator.update_and_settle(GROUP, &identifier, &patch).await;
        assert_eq!(
            text(&settled, "OperationStatus"),
            STATUS_SUCCESS,
            "case {case:?}: expected the patch to succeed, got {settled:?}"
        );
        let model = emulator.properties(GROUP, &identifier).await;
        assert_eq!(
            model.get("RetentionInDays"),
            Some(&json!(expected)),
            "case {case:?}: expected RetentionInDays {expected}, got {model:?}"
        );
    }
}

#[tokio::test]
async fn a_write_only_property_is_refused_every_operation_but_add() {
    let emulator = Emulator::with_ssm();
    let name = "write-only-parameter";
    let identifier = emulator
        .provision(
            PARAMETER,
            &json!({ "Name": name, "Type": "String", "Value": "v1" }),
        )
        .await;

    // The rows differ along the operation that names the property, the property named (one
    // the created parameter's model carries, one it does not, and two at once — which the
    // diagnostic lists together), and whether the value travels with the operation.
    // Catalog source: the `writeOnlyProperties` of the `AWS::SSM::Parameter` resource
    // provider schema, the properties `ssm:GetParameter` does not report.
    let rows: [(&str, Value, &str); 6] = [
        (
            "replacing a write-only property the model does not carry",
            json!([{ "op": "replace", "path": "/Description", "value": "later" }]),
            "/Description",
        ),
        (
            "replacing a write-only property the model does carry",
            json!([{ "op": "replace", "path": "/Tier", "value": "Advanced" }]),
            "/Tier",
        ),
        (
            "removing one",
            json!([{ "op": "remove", "path": "/AllowedPattern" }]),
            "/AllowedPattern",
        ),
        (
            "moving a value into one",
            json!([{ "op": "move", "from": "/Value", "path": "/Policies" }]),
            "/Policies",
        ),
        (
            "copying a value into one",
            json!([{ "op": "copy", "from": "/Value", "path": "/Description" }]),
            "/Description",
        ),
        (
            "naming two write-only properties at once",
            json!([
                { "op": "replace", "path": "/Tier", "value": "Advanced" },
                { "op": "remove", "path": "/Description" },
            ]),
            "/Description, /Tier",
        ),
    ];

    for (case, patch, pointers) in rows {
        let before = emulator.properties(PARAMETER, &identifier).await;
        let (status, body) = emulator.update(PARAMETER, &identifier, &patch, None).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body:?}"
        );
        assert!(
            code(&body).contains("ValidationException"),
            "case {case:?}: expected ValidationException, got {:?}",
            code(&body)
        );
        assert_eq!(
            message(&body),
            format!(
                "Invalid patch update: writeOnlyProperties [{pointers}] can only be updated \
                 using 'add' operation"
            ),
            "case {case:?}: unexpected wording, got {:?}",
            message(&body)
        );
        let after = emulator.properties(PARAMETER, &identifier).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused patch must change nothing, got {after:?}"
        );
    }

    // `add` is the one operation that sets a write-only property...
    let settled = emulator
        .update_and_settle(
            PARAMETER,
            &identifier,
            &json!([{ "op": "add", "path": "/Description", "value": "set by an add" }]),
        )
        .await;
    assert_eq!(
        text(&settled, "OperationStatus"),
        STATUS_SUCCESS,
        "an add of a write-only property must succeed, got {settled:?}"
    );

    // ...and it stays the one operation that may: the refusal is about the DOCUMENT, so it
    // does not depend on what the stored model carries now.
    let (status, body) = emulator
        .update(
            PARAMETER,
            &identifier,
            &json!([{ "op": "replace", "path": "/Description", "value": "replaced" }]),
            None,
        )
        .await;
    assert_eq!(
        status, 400,
        "replacing a write-only property that has been set: expected HTTP 400, got {status} \
         with body {body:?}"
    );
    assert_eq!(
        message(&body),
        "Invalid patch update: writeOnlyProperties [/Description] can only be updated using \
         'add' operation",
        "replacing a write-only property that has been set: unexpected wording, got {:?}",
        message(&body)
    );

    // A property the schema does NOT declare write-only is patched as usual, so the refusals
    // above are about the schema rather than about patching at all.
    let settled = emulator
        .update_and_settle(
            PARAMETER,
            &identifier,
            &json!([{ "op": "replace", "path": "/Value", "value": "v2" }]),
        )
        .await;
    assert_eq!(
        text(&settled, "OperationStatus"),
        STATUS_SUCCESS,
        "replacing a property the schema does not declare write-only must succeed, got \
         {settled:?}"
    );
    let model = emulator.properties(PARAMETER, &identifier).await;
    assert_eq!(
        model.get("Value"),
        Some(&json!("v2")),
        "expected the patched value to be read back, got {model:?}"
    );
}
