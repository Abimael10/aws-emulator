// SPDX-License-Identifier: CC0-1.0

//! The CONSTRAINT SET a refusal of this service states, end to end: how the expression a
//! `@pattern` requirement quotes is rendered, how many clauses one offending member gets,
//! and where a constraint broken by a collection's elements is stated.
//!
//! Three rules are pinned here, each over a TABLE spanning several operations, members and
//! constraint classes rather than the one request a recorded unit happened to send:
//!
//! 1. A `@pattern` clause quotes the expression the service PUBLISHES for the member's
//!    shape — the model's text without the whole-value `^`…`$` pair a model writes around a
//!    published expression — and the model's text verbatim for the shapes this service
//!    publishes as declared.
//! 2. A member's clauses are the WHOLE constraint set the service enforces on that member's
//!    shape, one clause per constraint the value broke, in the order that set states them:
//!    so a value outside an expression AND outside a length band is two validation errors,
//!    and a value outside only the half this checkout's model does not declare is still
//!    refused.
//! 3. A constraint broken by a collection's ELEMENTS is stated ONCE at the collection
//!    member, with the element shape's whole set nested inside
//!    `Member must satisfy constraint: [ … ]` — never at a per-element path, and never
//!    split into one error per constraint.
//!
//! A fourth test holds the other direction: a value the stated set ADMITS reaches the
//! handler, so widening what a clause states never widens what is refused.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router`], [`call`]) dispatches `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service router.

use serde_json::{json, Value};

use crate::parameter_store::{call, error_code, error_message, router};

/// A managed node id of the longer EC2 form, which every constraint of the node-id set
/// admits.
const NODE: &str = "i-0abcdef0123456789";

/// The envelope AWS counts `clauses` in, as the framework renders it. Shared with the
/// sibling suites that word the same envelope (`all/cloud_connector_input_constraint_wording`).
pub(crate) fn detected(clauses: &[String]) -> String {
    match clauses {
        [one] => format!("1 validation error detected: {one}"),
        many => format!(
            "{} validation errors detected: {}",
            many.len(),
            many.join("; ")
        ),
    }
}

/// One clause about `path`, quoting the value the request carried. Shared, as [`detected`] is.
pub(crate) fn clause(value: &str, path: &str, requirement: &str) -> String {
    format!("Value '{value}' at '{path}' failed to satisfy constraint: {requirement}")
}

/// One clause about `path` with no value at all, which the operations whose refusals state
/// none report — and which every clause stated at a collection member of a member those
/// recordings cover reports.
fn valueless_clause(path: &str, requirement: &str) -> String {
    format!("Value at '{path}' failed to satisfy constraint: {requirement}")
}

/// The requirement clause for a value outside its member's `@pattern`. Shared, as
/// [`detected`] is.
pub(crate) fn pattern(expression: &str) -> String {
    format!("Member must satisfy regular expression pattern: {expression}")
}

/// The requirement clause for a value shorter than its member's floor.
fn at_least(length: usize) -> String {
    format!("Member must have length greater than or equal to {length}")
}

/// The requirement clause for a value longer than its member's ceiling.
fn at_most(length: usize) -> String {
    format!("Member must have length less than or equal to {length}")
}

/// The requirement a clause stated at a COLLECTION member nests: the element shape's whole
/// set.
fn element_set(requirements: &[String]) -> String {
    format!(
        "Member must satisfy constraint: [{}]",
        requirements.join(", ")
    )
}

/// The expression the service publishes for a managed node id — the model's text
/// (`^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$`) without the pair it wraps that expression in.
const NODE_ID_EXPRESSION: &str = r"(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)";

/// The element set stated at a list of managed node ids: the band the service holds an id
/// to, its expression, and the presence every element of a list that is not `@sparse` has.
fn node_id_element_set() -> String {
    element_set(&[
        at_most(20),
        at_least(10),
        pattern(NODE_ID_EXPRESSION),
        "Member must not be null".to_string(),
    ])
}

/// Refuse `body` under `operation` and return the message, asserting the status and the
/// error code every one of these refusals carries.
async fn refusal(case: &str, operation: &str, body: &Value) -> String {
    let router = router();
    let (status, response) = call(&router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: a broken constraint is refused with 400, got {status} {response}"
    );
    assert_eq!(
        error_code(&response),
        "ValidationException",
        "case {case:?}: the refusal is a ValidationException, got {response}"
    );
    error_message(&response)
}

/// RULE 1: the expression a `@pattern` clause quotes, over four shapes and five operations —
/// a UUID, two spellings of a resource ARN, an activation id the recordings do not cover and
/// a document name.
#[tokio::test]
async fn a_pattern_clause_quotes_the_published_expression() {
    /// The expression the service publishes for an OpsMetadata ARN.
    const OPS_METADATA_ARN: &str =
        r"arn:(aws[a-zA-Z-]*)?:ssm:[a-z0-9-\.]{0,63}:[a-z0-9-\.]{0,63}:opsmetadata\/([a-zA-Z0-9-_\.\/]*)";

    // (case, operation, body, the clause the refusal states)
    let cases: Vec<(&str, &str, Value, String)> = vec![
        (
            "an inventory deletion id that is not a UUID",
            "DescribeInventoryDeletions",
            json!({ "DeletionId": "not-a-valid-deletion-id" }),
            clause(
                "not-a-valid-deletion-id",
                "deletionId",
                &pattern("[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}"),
            ),
        ),
        (
            "an OpsMetadata ARN that is not an ARN at all",
            "GetOpsMetadata",
            json!({ "OpsMetadataArn": "not-an-arn" }),
            clause("not-an-arn", "opsMetadataArn", &pattern(OPS_METADATA_ARN)),
        ),
        (
            "an ARN of another resource type, on the update",
            "UpdateOpsMetadata",
            json!({
                "OpsMetadataArn": "arn:aws:ssm:us-east-1:123456789012:parameter/not-opsmetadata",
            }),
            clause(
                "arn:aws:ssm:us-east-1:123456789012:parameter/not-opsmetadata",
                "opsMetadataArn",
                &pattern(OPS_METADATA_ARN),
            ),
        ),
        (
            // `DeleteActivation$ActivationId`
            // (`smithy_query(query="operation",operation="DeleteActivation")`), a member no
            // recorded refusal of this service covers: the rendering belongs to the shape,
            // so it holds here too.
            "an activation id that is not a UUID",
            "DeleteActivation",
            json!({ "ActivationId": "not-an-activation" }),
            clause(
                "not-an-activation",
                "activationId",
                &pattern("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"),
            ),
        ),
        (
            // `DescribeDocument$Name` targets `DocumentARN`, whose expression bounds the
            // characters AND the length of a name.
            "a document name too short for its expression",
            "DescribeDocument",
            json!({ "Name": "x" }),
            clause("x", "name", &pattern(r"[a-zA-Z0-9_\-.:/]{3,128}")),
        ),
    ];

    for (case, operation, body, clause) in cases {
        let stated = refusal(case, operation, &body).await;
        let expected = detected(&[clause]);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
    }
}

/// RULE 2: every constraint of the member's stated set the value broke gets its own clause,
/// in the order the set states them — over five shapes, eight operations, three constraint
/// classes and values that break one half of a set or both.
#[tokio::test]
async fn a_member_states_every_constraint_its_value_broke() {
    /// The expression the service publishes for a command id, which it states as the model
    /// declares one.
    const COMMAND_ID: &str =
        "^[A-Fa-f0-9]{8}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{12}$";

    /// The expression the service publishes for a document version.
    const DOCUMENT_VERSION: &str = r"([$]LATEST|[$]DEFAULT|^[1-9][0-9]*$)";

    // (case, operation, body, the clauses the refusal states, in order)
    let cases: Vec<(&str, &str, Value, Vec<String>)> = vec![
        (
            "a command id that is neither a UUID nor long enough",
            "GetCommandInvocation",
            json!({ "CommandId": "not-a-command-id", "InstanceId": NODE }),
            vec![
                clause("not-a-command-id", "commandId", &pattern(COMMAND_ID)),
                clause("not-a-command-id", "commandId", &at_least(36)),
            ],
        ),
        (
            // The half this checkout's model does not declare, on its own: a value of
            // exactly the modelled length that is not the form the service requires.
            "a command id of the modelled length that is not a UUID",
            "GetCommandInvocation",
            json!({
                "CommandId": "not-a-command-id-but-36-characters!!",
                "InstanceId": NODE,
            }),
            vec![clause(
                "not-a-command-id-but-36-characters!!",
                "commandId",
                &pattern(COMMAND_ID),
            )],
        ),
        (
            "a node id too short for the band and outside the expression",
            "ListInventoryEntries",
            json!({ "InstanceId": "i-zzz", "TypeName": "AWS:Application" }),
            vec![
                clause("i-zzz", "instanceId", &at_least(10)),
                clause("i-zzz", "instanceId", &pattern(NODE_ID_EXPRESSION)),
            ],
        ),
        (
            "the same node id on the patch read",
            "DescribeInstancePatches",
            json!({ "InstanceId": "i-zzz" }),
            vec![
                clause("i-zzz", "instanceId", &at_least(10)),
                clause("i-zzz", "instanceId", &pattern(NODE_ID_EXPRESSION)),
            ],
        ),
        (
            // `DescribeEffectiveInstanceAssociations$InstanceId`, an operation no recorded
            // refusal of this class covers: the set belongs to the shape.
            "the same node id on an association read",
            "DescribeEffectiveInstanceAssociations",
            json!({ "InstanceId": "mi-short" }),
            vec![
                clause("mi-short", "instanceId", &at_least(10)),
                clause("mi-short", "instanceId", &pattern(NODE_ID_EXPRESSION)),
            ],
        ),
        (
            // The execution preview states no value in any clause of its refusals.
            "a document version that is neither a label nor a number",
            "StartExecutionPreview",
            json!({
                "DocumentName": "AWS-RunShellScript",
                "DocumentVersion": "not-a-version",
            }),
            vec![
                valueless_clause("documentVersion", &pattern(DOCUMENT_VERSION)),
                valueless_clause("documentVersion", &at_most(8)),
            ],
        ),
        (
            // The length half on its own, on an operation that quotes its values: nine
            // digits satisfy the expression and not the length the service holds a version
            // to.
            "a version number one digit past the length",
            "DescribeDocument",
            json!({ "Name": "AWS-RunShellScript", "DocumentVersion": "123456789" }),
            vec![clause("123456789", "documentVersion", &at_most(8))],
        ),
        (
            // The document NAME of the execution preview: the service publishes a wider
            // expression for that parameter than the shape this checkout's model gives every
            // member targeting it, and a clause states the published one.
            "a document name too short for the expression the preview publishes",
            "StartExecutionPreview",
            json!({ "DocumentName": "x" }),
            vec![valueless_clause(
                "documentName",
                &pattern(r"^[a-zA-Z0-9_\-.:/]{3,200}$"),
            )],
        ),
        (
            // A `@range` ceiling: the live service serves a smaller page than the maximum
            // this checkout's model declares for the shape, and states its own ceiling.
            "a patch page size past the ceiling the service serves",
            "DescribeInstancePatches",
            json!({ "InstanceId": NODE, "MaxResults": 101 }),
            vec![clause(
                "101",
                "maxResults",
                "Member must have value less than or equal to 50",
            )],
        ),
        (
            // The half the model does not declare, on its own: a page size the model admits
            // and the service does not.
            "a patch page size the model admits and the service does not",
            "DescribeInstancePatches",
            json!({ "InstanceId": NODE, "MaxResults": 60 }),
            vec![clause(
                "60",
                "maxResults",
                "Member must have value less than or equal to 50",
            )],
        ),
        (
            // The OpsItem event listing states no value in any clause of its refusals
            // either, whatever constraint class was broken.
            "an OpsItem event filter operator outside its enumerated type",
            "ListOpsItemEvents",
            json!({
                "Filters": [{
                    "Key": "OpsItemId",
                    "Values": ["oi-0123456789ab"],
                    "Operator": "GreaterThan",
                }],
            }),
            vec![valueless_clause(
                "filters.1.member.operator",
                "Member must satisfy enum value set: [Equal]",
            )],
        ),
    ];

    for (case, operation, body, clauses) in cases {
        let stated = refusal(case, operation, &body).await;
        let expected = detected(&clauses);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
    }
}

/// RULE 3: a constraint broken by a collection's elements is ONE validation error, stated at
/// the collection member with the element shape's whole set nested inside it — over two
/// element shapes, four operations, one and two offending elements, and a list whose first
/// element is fine.
#[tokio::test]
async fn a_collections_elements_are_stated_at_the_collection_member() {
    /// The element set of an OpsItem event filter's values, whose expression this service
    /// publishes as the model declares it.
    fn ops_item_id_element_set() -> String {
        element_set(&[
            at_most(15),
            at_least(1),
            pattern("^(oi)-[0-9a-f]{12}$"),
        ])
    }

    /// One well-formed OpsItem event filter, for the cases that refuse a LATER filter.
    fn filter(value: &str) -> Value {
        json!({ "Key": "OpsItemId", "Values": [value], "Operator": "Equal" })
    }

    // (case, operation, body, the one clause the refusal states)
    let cases: Vec<(&str, &str, Value, String)> = vec![
        (
            "a node id list holding one value that is not a node id",
            "SendCommand",
            json!({
                "DocumentName": "AWS-RunShellScript",
                "InstanceIds": ["not-an-instance-id"],
            }),
            clause(
                "[not-an-instance-id]",
                "instanceIds",
                &node_id_element_set(),
            ),
        ),
        (
            "the same list on the patch-state read",
            "DescribeInstancePatchStates",
            json!({ "InstanceIds": ["i-zzz"] }),
            clause("[i-zzz]", "instanceIds", &node_id_element_set()),
        ),
        (
            "TWO offending elements, which are still one validation error",
            "DescribeInstancePatchStates",
            json!({ "InstanceIds": ["i-zzz", "mi-also-bad"] }),
            clause(
                "[i-zzz, mi-also-bad]",
                "instanceIds",
                &node_id_element_set(),
            ),
        ),
        (
            "a list whose FIRST element is a node id and whose second is not",
            "DescribeInstancePatchStates",
            json!({ "InstanceIds": [NODE, "nope"] }),
            clause(
                &format!("[{NODE}, nope]"),
                "instanceIds",
                &node_id_element_set(),
            ),
        ),
        (
            "an OpsItem event filter value that is not an OpsItem id",
            "ListOpsItemEvents",
            json!({ "Filters": [filter("not-a-valid-ops-item-id")] }),
            valueless_clause("filters.1.member.values", &ops_item_id_element_set()),
        ),
        (
            "the same value in the SECOND filter, which keeps its position",
            "ListOpsItemEvents",
            json!({ "Filters": [filter("oi-0123456789ab"), filter("nope")] }),
            valueless_clause("filters.2.member.values", &ops_item_id_element_set()),
        ),
        (
            // `GetParameters$Names` targets `ParameterNameList`, whose elements carry a
            // `@length` and no expression
            // (`smithy_query(query="shape",shape="ParameterNameList")`) — a collection no
            // recorded refusal of this class covers, stated from the set the MODEL declares.
            "a parameter name list holding an empty name",
            "GetParameters",
            json!({ "Names": ["", "/db/password"] }),
            clause(
                "[, /db/password]",
                "names",
                &element_set(&[at_most(2048), at_least(1)]),
            ),
        ),
    ];

    for (case, operation, body, clause) in cases {
        let stated = refusal(case, operation, &body).await;
        let expected = detected(&[clause]);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
        assert!(
            !stated.contains(".member'"),
            "case {case:?}: a grouped clause names the collection member, not an element, \
             got {stated:?}"
        );
    }
}

/// The other direction of rules 2 and 3: every value a stated set ADMITS reaches the
/// handler, so a refusal this service widened never refuses a value the service accepts.
///
/// What the handler then answers is that operation's own business — the modelled error for a
/// resource no Region holds, a served response, or the scaffold's 501 for an operation this
/// emulator has not implemented yet — and never `ValidationException`, which is the boundary
/// refusal under test.
#[tokio::test]
async fn a_value_the_stated_set_admits_is_not_refused_at_the_boundary() {
    let router = router();

    /// What the handler answers a request the boundary did not refuse.
    enum Answered {
        /// The request is served.
        Served,
        /// The operation's own modelled error, for a resource no Region holds.
        Modelled(&'static str),
        /// Whatever the operation answers, which for one still carrying the scaffold is a
        /// 501: this rule is only about the boundary not refusing the value.
        Anything,
    }

    // (case, operation, body, what the handler answers)
    let cases: Vec<(&str, &str, Value, Answered)> = vec![
        (
            "a command id of the form this service mints",
            "GetCommandInvocation",
            json!({
                "CommandId": "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0",
                "InstanceId": NODE,
            }),
            Answered::Modelled("InvalidCommandId"),
        ),
        (
            "the shorter EC2 node id, at the band's floor",
            "DescribeInstancePatches",
            json!({ "InstanceId": "i-0abcdef0" }),
            Answered::Anything,
        ),
        (
            "a hybrid node id, at the band's ceiling",
            "DescribeInstancePatches",
            json!({ "InstanceId": "mi-0abcdef0123456789" }),
            Answered::Anything,
        ),
        (
            "a list of node ids of both forms",
            "DescribeInstancePatchStates",
            json!({ "InstanceIds": [NODE, "mi-0abcdef0123456789"] }),
            Answered::Anything,
        ),
        (
            "a version number the length admits",
            "DescribeDocument",
            json!({ "Name": "NotADocumentOfThisRegion", "DocumentVersion": "12345678" }),
            Answered::Modelled("InvalidDocument"),
        ),
        (
            "the default version label, at the length ceiling",
            "DescribeDocument",
            json!({ "Name": "NotADocumentOfThisRegion", "DocumentVersion": "$DEFAULT" }),
            Answered::Modelled("InvalidDocument"),
        ),
        (
            "an OpsItem id the filter's expression admits",
            "ListOpsItemEvents",
            json!({
                "Filters": [{
                    "Key": "OpsItemId",
                    "Values": ["oi-0123456789ab"],
                    "Operator": "Equal",
                }],
            }),
            Answered::Served,
        ),
    ];

    for (case, operation, body, answered) in cases {
        let (status, response) = call(&router, operation, &body).await;
        let stated = error_code(&response);
        assert_ne!(
            stated, "ValidationException",
            "case {case:?}: a value the stated set admits is not refused at the boundary, \
             got {status} {}",
            error_message(&response)
        );
        match answered {
            Answered::Served => assert_eq!(
                status, 200,
                "case {case:?}: the request is served, got {status} {response}"
            ),
            Answered::Modelled(modelled) => assert_eq!(
                stated, modelled,
                "case {case:?}: expected the operation's own {modelled}, got {status} \
                 {response}"
            ),
            Answered::Anything => {}
        }
    }
}
