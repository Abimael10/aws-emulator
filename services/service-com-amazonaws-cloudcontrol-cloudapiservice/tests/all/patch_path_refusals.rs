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
//!
//! The write-only rule is held over TWO types, because the properties it bears on are each
//! type's schema's own: `AWS::SSM::Parameter`, whose write-only properties are configuration
//! a parameter carries, and `AWS::StepFunctions::StateMachine`, whose write-only `Definition`
//! is the workflow itself — so a type's write-only properties are catalogued from its schema
//! rather than the rule holding for one type.

use serde_json::{json, Value};

use super::emulator::{code, message, text, Emulator, STATUS_SUCCESS};

/// A queue type, created with nothing but its name so the model carries few properties.
const QUEUE: &str = "AWS::SQS::Queue";

/// A table type, whose model carries nested objects and a list to point into. One no
/// provider of this emulator stands behind, so its model is exactly what the desired state
/// below spells.
const TABLE: &str = "AWS::Timestream::Table";

/// A log group type, whose schema's `RetentionInDays` is a property the owning service
/// reports only once something has set it.
const GROUP: &str = "AWS::Logs::LogGroup";

/// A parameter type, the one whose schema declares write-only properties.
const PARAMETER: &str = "AWS::SSM::Parameter";

/// A state machine type, whose schema declares the definition a request SETS write-only:
/// the workflow the owning service runs is reported under another property, so the model a
/// patch is applied to carries nothing at that pointer.
const STATE_MACHINE: &str = "AWS::StepFunctions::StateMachine";

/// The write-only properties of that type, in the order its schema declares them.
///
/// Catalog source: the `writeOnlyProperties` of the `AWS::StepFunctions::StateMachine`
/// resource provider schema — the three forms a request states the Amazon States Language
/// definition in beside `DefinitionString`, none of which `states:DescribeStateMachine`
/// reports.
const STATE_MACHINE_WRITE_ONLY: [&str; 3] = [
    "Definition",
    "DefinitionS3Location",
    "DefinitionSubstitutions",
];

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

/// One Amazon States Language definition, as the schema's `Definition` object carries it.
fn definition(version: u64) -> Value {
    json!({
        "Comment": format!("v{version}"),
        "StartAt": "P",
        "States": { "P": { "Type": "Pass", "Result": { "version": version }, "End": true } },
    })
}

/// What the OWNING service holds for the state machine `identifier`: the definition it runs
/// and the revision it is at, which is where a refused patch has to leave them.
async fn described(emulator: &Emulator, identifier: &str) -> (String, String) {
    let read = emulator
        .sfn_ok(
            "DescribeStateMachine",
            json!({ "stateMachineArn": identifier }),
        )
        .await;
    (text(&read, "definition"), text(&read, "revisionId"))
}

/// One row of the state machine's write-only table: what it is about, the patch document,
/// and the pointers the refusal must name.
struct WriteOnly {
    /// What this row is about.
    case: &'static str,
    /// The patch document the request carries.
    patch: Value,
    /// The pointers the refusal lists, in the order the schema declares the properties.
    pointers: &'static str,
}

#[tokio::test]
async fn a_write_only_definition_is_refused_every_operation_but_add() {
    // The list the gate judges a document by is the schema's own, so the cases below hold
    // the service to the resource type's schema rather than to a catalogue of its own.
    let catalogued =
        service_com_amazonaws_cloudcontrol_cloudapiservice::state::write_only_properties(
            STATE_MACHINE,
        );
    assert_eq!(
        catalogued, STATE_MACHINE_WRITE_ONLY,
        "expected the schema's writeOnlyProperties {STATE_MACHINE_WRITE_ONLY:?}, got \
         {catalogued:?}"
    );

    let emulator = Emulator::with_step_functions();
    let role_arn = engine::emulator_arn("iam", "", "role/states-execution");
    let created = definition(1);
    let identifier = emulator
        .provision(
            STATE_MACHINE,
            &json!({
                "StateMachineName": "write-only-definition",
                "RoleArn": role_arn,
                "Definition": created,
                "StateMachineType": "STANDARD",
            }),
        )
        .await;
    // What the owning service holds before any patch is sent: the definition the create
    // carried and the revision it is at, neither of which a refused patch may move.
    let before_native = described(&emulator, &identifier).await;
    assert_eq!(
        before_native.0,
        created.to_string(),
        "the created definition must be the one Step Functions holds, got {before_native:?}"
    );

    // The rows differ along the operation that names the property, the DEPTH of the pointer,
    // which write-only property of the type is named, and how many of them one document
    // names — which the diagnostic lists together, in the schema's order.
    let rows = [
        WriteOnly {
            case: "replacing the write-only definition the model does not carry",
            patch: json!([{ "op": "replace", "path": "/Definition", "value": definition(2) }]),
            pointers: "/Definition",
        },
        WriteOnly {
            case: "replacing a member INSIDE it, which is the same property",
            patch: json!([{ "op": "replace", "path": "/Definition/StartAt", "value": "Q" }]),
            pointers: "/Definition",
        },
        WriteOnly {
            case: "removing it",
            patch: json!([{ "op": "remove", "path": "/Definition" }]),
            pointers: "/Definition",
        },
        WriteOnly {
            case: "moving the reported definition into it",
            patch: json!([
                { "op": "move", "from": "/DefinitionString", "path": "/Definition" },
            ]),
            pointers: "/Definition",
        },
        WriteOnly {
            case: "copying the reported definition into it",
            patch: json!([
                { "op": "copy", "from": "/DefinitionString", "path": "/Definition" },
            ]),
            pointers: "/Definition",
        },
        WriteOnly {
            case: "replacing another write-only property of the same type",
            patch: json!([
                { "op": "replace", "path": "/DefinitionSubstitutions", "value": { "K": "v" } },
            ]),
            pointers: "/DefinitionSubstitutions",
        },
        WriteOnly {
            case: "a document whose SECOND operation names a write-only property",
            patch: json!([
                { "op": "add", "path": "/Tags", "value": [{ "Key": "team", "Value": "ops" }] },
                { "op": "remove", "path": "/DefinitionS3Location" },
            ]),
            pointers: "/DefinitionS3Location",
        },
        WriteOnly {
            case: "naming two write-only properties at once",
            patch: json!([
                { "op": "remove", "path": "/DefinitionS3Location" },
                { "op": "replace", "path": "/Definition", "value": definition(3) },
            ]),
            pointers: "/Definition, /DefinitionS3Location",
        },
    ];

    for row in rows {
        let before = emulator.properties(STATE_MACHINE, &identifier).await;
        let (status, body) = emulator
            .update(STATE_MACHINE, &identifier, &row.patch, None)
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
                "Invalid patch update: writeOnlyProperties [{}] can only be updated using \
                 'add' operation",
                row.pointers
            ),
            "case {case:?}: unexpected wording, got {:?}",
            message(&body)
        );
        let after = emulator.properties(STATE_MACHINE, &identifier).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused patch must change nothing, got {after:?}"
        );
        let after_native = described(&emulator, &identifier).await;
        assert_eq!(
            after_native, before_native,
            "case {case:?}: the owning service must still report the definition and the \
             revision it held, got {after_native:?}"
        );
    }

    // `add` is the one operation that SETS a write-only property, and what it sets is what
    // the owning service then runs — read back through its own API and through `GetResource`,
    // which reports the definition under the property a read answers with.
    let wanted = definition(4);
    let settled = emulator
        .update_and_settle(
            STATE_MACHINE,
            &identifier,
            &json!([{ "op": "add", "path": "/Definition", "value": wanted }]),
        )
        .await;
    assert_eq!(
        text(&settled, "OperationStatus"),
        STATUS_SUCCESS,
        "an add of the write-only definition must succeed, got {settled:?}"
    );
    let added_native = described(&emulator, &identifier).await;
    assert_eq!(
        added_native.0,
        wanted.to_string(),
        "the added definition must be the one Step Functions then holds, got {added_native:?}"
    );
    let model = emulator.properties(STATE_MACHINE, &identifier).await;
    assert_eq!(
        model.get("DefinitionString"),
        Some(&json!(wanted.to_string())),
        "the model must report the added definition as DefinitionString, got {model:?}"
    );
    assert_eq!(
        model.get("Definition"),
        None,
        "a write-only property is part of no model this service reports, got {model:?}"
    );

    // A property of the SAME type the schema does not declare write-only is replaced as
    // usual, so the refusals above are about the schema's list rather than about the type or
    // about patching a definition at all.
    let replaced = definition(5);
    let settled = emulator
        .update_and_settle(
            STATE_MACHINE,
            &identifier,
            &json!([{
                "op": "replace",
                "path": "/DefinitionString",
                "value": replaced.to_string(),
            }]),
        )
        .await;
    assert_eq!(
        text(&settled, "OperationStatus"),
        STATUS_SUCCESS,
        "replacing a property the schema does not declare write-only must succeed, got \
         {settled:?}"
    );
    let replaced_native = described(&emulator, &identifier).await;
    assert_eq!(
        replaced_native.0,
        replaced.to_string(),
        "the replaced definition must be the one Step Functions then holds, got \
         {replaced_native:?}"
    );
}
