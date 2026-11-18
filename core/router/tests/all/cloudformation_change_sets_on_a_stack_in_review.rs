// SPDX-License-Identifier: CC0-1.0

//! The change-set REVIEW flow over AWS Systems Manager parameters: creating one change set
//! after another on the stack a change set brought into existence, until one is worth
//! executing.
//!
//! The rule under test: **a stack in `REVIEW_IN_PROGRESS` is one a change set created and
//! nothing has executed, so it holds no template and no resources; a change set of a type
//! that BRINGS a stack into existence (`CREATE`, `IMPORT`) is therefore accepted on it,
//! targets that same stack id and is planned normally, however the change sets already on
//! it settled — while only a change set that plans an UPDATE of a revision the stack does
//! not have is refused, in the sentence CloudFormation refuses an update of such a stack
//! with.**
//!
//! Two crates have to agree for that to hold — CloudFormation plans and executes the import,
//! and Systems Manager owns the parameter and is the only side that can say whether one was
//! imported untouched or never existed — so the suite lives here, over the assembled
//! emulator, rather than in either of them.
//!
//! The rows differ along what the rule generalises over: how the FIRST change set on the
//! stack settled (failed on an identifier nothing holds, failed on a resource another stack
//! manages, or complete and executable), which type the second change set states, and how
//! the second call names the stack (by its name, and by the stack id the first call
//! answered).
//!
//! The XML readers and the form encoder are this crate's shared ones (`tests/all/query_wire.rs`).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{blocks_of, element, elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for, and so the Region every stack and parameter
/// lives in.
const REGION: &str = "us-east-1";

/// How long a wait for a clock-owed transition may take before the test fails rather than
/// hangs: far above the span itself, so neither a loaded host nor a busy test run can decide
/// the outcome. Nothing here asserts how long anything took.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long a wait sleeps between two looks at the resource it is waiting on.
const SETTLE_INTERVAL: Duration = Duration::from_millis(20);

/// The value a client holds a parameter under before any stack is created — different from
/// the one the template declares, so an import that rewrote the resource would be visible.
const OUT_OF_BAND_VALUE: &str = "written-out-of-band";

/// The value every template here declares.
const TEMPLATE_VALUE: &str = "from-the-template";

/// The change set each row creates first, and the one it creates second on the same stack.
const FIRST: &str = "cs1";
const SECOND: &str = "cs2";

/// The logical id of the only resource every template here declares.
const LOGICAL_ID: &str = "P";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request and what each handler reads its Region from. The signature is never
/// checked: this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(String, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation call that must be accepted.
async fn cloudformation_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(String, String)],
) -> String {
    let (status, body) = cloudformation(router, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with {body}"
    );
    body
}

/// Call one AWS Systems Manager operation (`sigv4Name: ssm`, service shape `AmazonSSM`) over
/// its awsJson1_1 wire.
async fn ssm(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ssm.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AmazonSSM.{operation}"))
        .header("authorization", authorization("ssm"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// Write one parameter through Systems Manager's own wire, so the name is held by a CLIENT
/// and no stack manages it.
async fn put_parameter(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, body) = ssm(
        router,
        "PutParameter",
        &json!({"Name": name, "Type": "String", "Value": OUT_OF_BAND_VALUE}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from PutParameter for {name:?}, got {status} with \
         {body}"
    );
}

/// The value Systems Manager holds under `name`, or `None` where it holds none.
async fn parameter_value(router: &MultiServiceRouter, name: &str) -> Option<String> {
    let (status, body) = ssm(router, "GetParameter", &json!({"Name": name})).await;
    if status != 200 {
        return None;
    }
    body.pointer("/Parameter/Value")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// A `DescribeChangeSet` answer read once the change set has left its creating statuses, by
/// LOOKING again until that holds or [`SETTLE_DEADLINE`] passes.
///
/// The wait is for the CONDITION rather than for a span, so a slower host only takes more
/// looks; the deadline is a safety net so a wait nothing will satisfy ends in a failure
/// rather than a hang.
async fn settled_change_set(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    change_set: &str,
) -> String {
    let members = vec![
        ("StackName".to_string(), stack.to_string()),
        ("ChangeSetName".to_string(), change_set.to_string()),
    ];
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let body = cloudformation_ok(router, case, "DescribeChangeSet", &members).await;
        let status = opt_element(&body, "Status").unwrap_or_default();
        if !status.ends_with("_PENDING") && !status.ends_with("_IN_PROGRESS") {
            return body;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the change set was still {status:?} after {SETTLE_DEADLINE:?}, so \
             nothing settled it: {body}"
        );
        tokio::time::sleep(SETTLE_INTERVAL).await;
    }
}

/// The `StackStatus` `DescribeStacks` reports for `stack` right now.
async fn stack_status(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let body = cloudformation_ok(
        router,
        case,
        "DescribeStacks",
        &[("StackName".to_string(), stack.to_string())],
    )
    .await;
    element(&body, "StackStatus")
}
/// The status `stack` rests in once it has reached `expected`, waited for the way
/// [`settled_change_set`] waits: by LOOKING again until it holds or [`SETTLE_DEADLINE`]
/// passes, never by sleeping for the span a transition takes.
async fn wait_for_stack_status(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    expected: &str,
) {
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let status = stack_status(router, case, stack).await;
        if status == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {stack} was {status:?} rather than {expected:?} after \
             {SETTLE_DEADLINE:?}, so nothing took it there"
        );
        tokio::time::sleep(SETTLE_INTERVAL).await;
    }
}

/// The plan a `DescribeChangeSet` answer carries, as `logical id -> (action, physical id)`.
///
/// Each pair is read from the `ResourceChange` block that carries them, rather than by
/// zipping document-ordered element lists: an addition reports no physical id at all, so a
/// zip would pair the wrong halves instead of showing an empty one.
fn plan_of(xml: &str) -> BTreeMap<String, (String, String)> {
    blocks_of(xml, "ResourceChange")
        .into_iter()
        .filter_map(|change| {
            let logical_id = opt_element(change, "LogicalResourceId")?;
            let action = opt_element(change, "Action")?;
            let physical_id = opt_element(change, "PhysicalResourceId").unwrap_or_default();
            Some((logical_id, (action, physical_id)))
        })
        .collect()
}

/// A template declaring one `AWS::SSM::Parameter` under `name`.
///
/// `Type` and `Value` are the type's two required properties and `Name` is the custom name it
/// is identified by (source: the CloudFormation resource reference for
/// `AWS::SSM::Parameter`). `retained` adds the `DeletionPolicy` CloudFormation requires of
/// every resource an IMPORT change set adopts.
fn parameter_template(name: &str, retained: bool) -> String {
    let policy = if retained {
        r#""DeletionPolicy":"Retain","#
    } else {
        ""
    };
    format!(
        r#"{{"Resources":{{"{LOGICAL_ID}":{{"Type":"AWS::SSM::Parameter",{policy}
            "Properties":{{"Name":"{name}","Type":"String","Value":"{TEMPLATE_VALUE}"}}}}}}}}"#
    )
}

/// The `ResourcesToImport` member list naming one parameter by the `Name` it is identified
/// by, as an SDK spells a list of structures on the awsQuery wire.
fn resources_to_import(name: &str) -> Vec<(String, String)> {
    vec![
        (
            "ResourcesToImport.member.1.ResourceType".to_string(),
            "AWS::SSM::Parameter".to_string(),
        ),
        (
            "ResourcesToImport.member.1.LogicalResourceId".to_string(),
            LOGICAL_ID.to_string(),
        ),
        (
            "ResourcesToImport.member.1.ResourceIdentifier.entry.1.key".to_string(),
            "Name".to_string(),
        ),
        (
            "ResourcesToImport.member.1.ResourceIdentifier.entry.1.value".to_string(),
            name.to_string(),
        ),
    ]
}

/// The members of one `CreateChangeSet` call, with the type and template a row states.
fn change_set_members(
    stack: &str,
    change_set: &str,
    change_set_type: &str,
    template: &str,
    imported: Option<&str>,
) -> Vec<(String, String)> {
    let mut members = vec![
        ("StackName".to_string(), stack.to_string()),
        ("ChangeSetName".to_string(), change_set.to_string()),
        ("ChangeSetType".to_string(), change_set_type.to_string()),
        ("TemplateBody".to_string(), template.to_string()),
    ];
    if let Some(name) = imported {
        members.extend(resources_to_import(name));
    }
    members
}

/// How the FIRST change set of a row settles — what the review flow has already tried on the
/// stack before the second call is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FirstOutcome {
    /// An IMPORT of an identifier nothing holds: no resource to adopt, so the creation fails.
    MissingIdentifier,
    /// An IMPORT of a resource ANOTHER stack manages: not this plan's to take.
    ManagedByAnotherStack,
    /// An IMPORT that is executable, and simply left unexecuted.
    Importable,
}

/// How the SECOND call names the stack the first one created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// By the name the first call stated.
    ByName,
    /// By the stack id the first call answered.
    ById,
}

/// What the rule says becomes of the second call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expected {
    /// Accepted, and planned as one change carrying this action; `Import` reports the
    /// identifier the resource already carries, while an addition reports none.
    Planned(&'static str),
    /// Refused as the update of a revision the stack does not hold.
    RefusedAsAnUpdate,
}

/// One row: what the stack has already been through, what the second call asks for, and what
/// the rule says the answer is.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The prefix this row's parameter names are built from, unique across the suite.
    prefix: &'static str,
    first: FirstOutcome,
    /// `ChangeSetType` as the second call states it.
    second_type: &'static str,
    target: Target,
    expected: Expected,
    /// Whether the row goes on to EXECUTE the plan it asserted, which only one row needs to
    /// do: the execution is the same for every accepted plan.
    executes: bool,
}

/// RULE: a stack in `REVIEW_IN_PROGRESS` holds no revision, so it takes further `CREATE` and
/// `IMPORT` change sets — planned against that same stack id, whatever became of the change
/// sets already on it — while a change set that plans an UPDATE of it is refused.
#[tokio::test]
async fn a_stack_in_review_takes_further_creating_change_sets() {
    let router = build_router();

    // A parameter ANOTHER stack manages, which that stack created: the name one row's first
    // change set tries to import and cannot.
    let owned = "/review/owned";
    let owner = cloudformation_ok(
        &router,
        "the owning stack",
        "CreateStack",
        &[
            ("StackName".to_string(), "review-owner".to_string()),
            (
                "TemplateBody".to_string(),
                parameter_template(owned, false),
            ),
        ],
    )
    .await;
    let owner_stack_id = element(&owner, "StackId");
    // The owning stack must hold the parameter before any row tries to import it.
    wait_for_stack_status(&router, "the owning stack", "review-owner", "CREATE_COMPLETE").await;

    let rows: Vec<Row> = vec![
        Row {
            case: "a failed import of a missing identifier, then an import by stack id",
            stack: "review-missing-by-id",
            prefix: "/review/missing-by-id",
            first: FirstOutcome::MissingIdentifier,
            second_type: "IMPORT",
            target: Target::ById,
            expected: Expected::Planned("Import"),
            executes: true,
        },
        Row {
            case: "a failed import of a missing identifier, then an import by stack name",
            stack: "review-missing-by-name",
            prefix: "/review/missing-by-name",
            first: FirstOutcome::MissingIdentifier,
            second_type: "IMPORT",
            target: Target::ByName,
            expected: Expected::Planned("Import"),
            executes: false,
        },
        Row {
            case: "a failed import of a resource another stack manages, then an import by \
                   stack id",
            stack: "review-managed",
            prefix: "/review/managed",
            first: FirstOutcome::ManagedByAnotherStack,
            second_type: "IMPORT",
            target: Target::ById,
            expected: Expected::Planned("Import"),
            executes: false,
        },
        Row {
            case: "an executable import left unexecuted, then another import on the same stack",
            stack: "review-executable",
            prefix: "/review/executable",
            first: FirstOutcome::Importable,
            second_type: "IMPORT",
            target: Target::ById,
            expected: Expected::Planned("Import"),
            executes: false,
        },
        Row {
            case: "a failed import, then a CREATE change set on the same stack",
            stack: "review-created",
            prefix: "/review/created",
            first: FirstOutcome::MissingIdentifier,
            second_type: "CREATE",
            target: Target::ById,
            expected: Expected::Planned("Add"),
            executes: false,
        },
        Row {
            case: "a failed import, then an UPDATE change set on the same stack",
            stack: "review-updated",
            prefix: "/review/updated",
            first: FirstOutcome::MissingIdentifier,
            second_type: "UPDATE",
            target: Target::ById,
            expected: Expected::RefusedAsAnUpdate,
            executes: false,
        },
    ];

    for row in &rows {
        let case = row.case;
        // The identifier the FIRST change set names, and the one the SECOND names. The second
        // is held by a client out of band for every row, so a plan that adopts it can be
        // compared with what Systems Manager holds afterwards.
        let missing = format!("{}/absent", row.prefix);
        let present = format!("{}/present", row.prefix);
        put_parameter(&router, case, &present).await;
        let (first_identifier, first_reason) = match row.first {
            // The fixed text CloudFormation answers for a resource to import that the owning
            // service does not hold, with the type and the identifier the request named.
            FirstOutcome::MissingIdentifier => (
                missing.clone(),
                Some(format!(
                    "Resource of type 'AWS::SSM::Parameter' with identifier '{missing}' was \
                     not found."
                )),
            ),
            // ...and the fixed text it answers for a name another stack manages, with that
            // stack's own id.
            FirstOutcome::ManagedByAnotherStack => (
                owned.to_string(),
                Some(format!("{owned} already exists in stack {owner_stack_id}")),
            ),
            FirstOutcome::Importable => (present.clone(), None),
        };
        // ...and where the first change set is the executable one, the second adopts a
        // DIFFERENT name, so the two plans are not the same plan.
        let second_identifier = match row.first {
            FirstOutcome::Importable => {
                let other = format!("{}/other", row.prefix);
                put_parameter(&router, case, &other).await;
                other
            }
            _ => present.clone(),
        };

        // The first change set brings the stack into existence, in review.
        let created = cloudformation_ok(
            &router,
            case,
            "CreateChangeSet",
            &change_set_members(
                row.stack,
                FIRST,
                "IMPORT",
                &parameter_template(&first_identifier, true),
                Some(&first_identifier),
            ),
        )
        .await;
        let stack_id = element(&created, "StackId");
        let settled = settled_change_set(&router, case, row.stack, FIRST).await;
        let expected_first = match &first_reason {
            Some(reason) => ("FAILED".to_string(), reason.clone()),
            None => ("CREATE_COMPLETE".to_string(), String::new()),
        };
        assert_eq!(
            (
                opt_element(&settled, "Status").unwrap_or_default(),
                // Read through the decoding reader: the reason quotes the type and the
                // identifier, which the XML serializer escapes.
                elements(&settled, "StatusReason")
                    .first()
                    .cloned()
                    .unwrap_or_default(),
            ),
            expected_first,
            "case {case:?}: expected the first change set to settle {expected_first:?}, got \
             {settled}"
        );
        let in_review = stack_status(&router, case, row.stack).await;
        assert_eq!(
            in_review, "REVIEW_IN_PROGRESS",
            "case {case:?}: the stack the first change set created must be in review, got \
             {in_review:?}"
        );

        // The second change set, on that same stack, named the way the row names it.
        let named = match row.target {
            Target::ByName => row.stack.to_string(),
            Target::ById => stack_id.clone(),
        };
        let (imports, template_name) = match row.second_type {
            "IMPORT" => (Some(second_identifier.as_str()), second_identifier.clone()),
            // A CREATE change set plans an ADDITION, so it declares a name nothing holds.
            _ => (None, format!("{}/added", row.prefix)),
        };
        let members = change_set_members(
            &named,
            SECOND,
            row.second_type,
            &parameter_template(&template_name, imports.is_some()),
            imports,
        );
        let (status, answered) = cloudformation(&router, "CreateChangeSet", &members).await;

        let action = match row.expected {
            Expected::RefusedAsAnUpdate => {
                // The sentence CloudFormation refuses an update of a stack whose status takes
                // none with: the stack's own id and the status it is in are its only variable
                // parts.
                let expected =
                    format!("Stack:{stack_id} is in REVIEW_IN_PROGRESS state and can not be updated.");
                assert_eq!(
                    (
                        status,
                        opt_element(&answered, "Code").unwrap_or_default(),
                        opt_element(&answered, "Message").unwrap_or_default(),
                    ),
                    (400, "ValidationError".to_string(), expected.clone()),
                    "case {case:?}: expected the refusal to read {expected:?}, got {status} \
                     with {answered}"
                );
                // The refused call leaves the stack and its one change set exactly as they
                // were.
                let listed = cloudformation_ok(
                    &router,
                    case,
                    "ListChangeSets",
                    &[("StackName".to_string(), stack_id.clone())],
                )
                .await;
                assert_eq!(
                    elements(&listed, "ChangeSetName"),
                    vec![FIRST.to_string()],
                    "case {case:?}: the refused call must store no plan, got {listed}"
                );
                continue;
            }
            Expected::Planned(action) => action,
        };

        assert_eq!(
            status, 200,
            "case {case:?}: a stack in review must take a {} change set, got {status} with \
             {answered}",
            row.second_type
        );
        assert_eq!(
            opt_element(&answered, "StackId").as_deref(),
            Some(stack_id.as_str()),
            "case {case:?}: the second change set must target the stack already in review, \
             got {answered}"
        );
        let settled = settled_change_set(&router, case, &stack_id, SECOND).await;
        // An adoption reports the identifier the resource ALREADY carries; an addition has
        // none, because the resource does not exist yet.
        let physical_id = match action {
            "Import" => second_identifier.clone(),
            _ => String::new(),
        };
        let expected_plan: BTreeMap<String, (String, String)> = BTreeMap::from([(
            LOGICAL_ID.to_string(),
            (action.to_string(), physical_id),
        )]);
        let plan = plan_of(&settled);
        assert_eq!(
            (
                opt_element(&settled, "Status").unwrap_or_default(),
                opt_element(&settled, "ExecutionStatus").unwrap_or_default(),
                plan.clone(),
            ),
            (
                "CREATE_COMPLETE".to_string(),
                "AVAILABLE".to_string(),
                expected_plan.clone()
            ),
            "case {case:?}: expected an executable plan reporting {expected_plan:?}, got \
             {plan:?} from {settled}"
        );
        // Both plans live on the one stack, and the stack is still the review-only one.
        let listed = cloudformation_ok(
            &router,
            case,
            "ListChangeSets",
            &[("StackName".to_string(), stack_id.clone())],
        )
        .await;
        let mut names = elements(&listed, "ChangeSetName");
        names.sort();
        assert_eq!(
            names,
            vec![FIRST.to_string(), SECOND.to_string()],
            "case {case:?}: the stack must carry both change sets, got {listed}"
        );
        let still_in_review = stack_status(&router, case, &stack_id).await;
        assert_eq!(
            still_in_review, "REVIEW_IN_PROGRESS",
            "case {case:?}: planning a change set must not deploy anything, got \
             {still_in_review:?}"
        );

        if !row.executes {
            continue;
        }
        // Executing the second plan is what the review flow was for: the stack adopts the
        // parameter it named, under the identifier it already had, and Systems Manager still
        // holds the value the client wrote.
        cloudformation_ok(
            &router,
            case,
            "ExecuteChangeSet",
            &[
                ("StackName".to_string(), stack_id.clone()),
                ("ChangeSetName".to_string(), SECOND.to_string()),
            ],
        )
        .await;
        // "An executed import completes as an import": the stack reaches the status an
        // import operation leaves it in, once the clock makes that transition due.
        wait_for_stack_status(&router, case, &stack_id, "IMPORT_COMPLETE").await;
        let resources = cloudformation_ok(
            &router,
            case,
            "DescribeStackResources",
            &[("StackName".to_string(), stack_id.clone())],
        )
        .await;
        let reported = physical_ids_of(&resources);
        let expected_ids: BTreeMap<String, String> =
            BTreeMap::from([(LOGICAL_ID.to_string(), second_identifier.clone())]);
        assert_eq!(
            reported, expected_ids,
            "case {case:?}: the stack must manage the imported name, expected \
             {expected_ids:?}, got {reported:?} from {resources}"
        );
        let held = parameter_value(&router, &second_identifier).await;
        assert_eq!(
            held.as_deref(),
            Some(OUT_OF_BAND_VALUE),
            "case {case:?}: importing {second_identifier:?} must leave it exactly as the \
             client wrote it, got {held:?}"
        );
    }
}
