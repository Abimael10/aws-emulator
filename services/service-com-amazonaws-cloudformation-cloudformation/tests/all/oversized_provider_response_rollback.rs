// SPDX-License-Identifier: CC0-1.0

//! What CloudFormation OWES ITSELF once a custom resource's provider has answered, and the
//! answer is one that fails the resource.
//!
//! RULE: a provider response document larger than the 4096 bytes the response protocol allows
//! is one CloudFormation does not read, so it fails that resource itself — `CREATE_FAILED`
//! with the fixed reason `Response object is too long.` — and the rollback that failure owes is
//! driven by CloudFormation, with NO further client call: the failed resource's own `Delete` is
//! published to its provider before any `Delete` for a resource it depends on, carrying the
//! `PhysicalResourceId` CloudFormation has for the resource at that moment (the one the
//! provider answered with where the answer was READ, and the one CloudFormation itself assigned
//! — `<stack name>-<logical id>-<unique id>` — where it was not), and the stack settles at
//! `ROLLBACK_COMPLETE`.
//!
//! The rows differ along what the rule generalises over: how far past the bound the document is
//! (one byte, well past it), what `Status` the unread document carries (so that a document
//! past the bound is not read whatever it says), and — the complement that fixes which id the
//! failure-path `Delete` names — an answer INSIDE the bound that CloudFormation did read and
//! that failed the resource with a physical id of the provider's own.
//!
//! Every assertion drives the emulator through its own wire: the stack operations over
//! CloudFormation's awsQuery endpoint and each provider answer as the plain, unsigned HTTP
//! `PUT` to the published `ResponseURL`, through the sequence
//! [`super::custom_resource_providers`] spells once. The window in which the rollback's own
//! request has to appear is polled on the STORE alone — not one CloudFormation operation is
//! called inside it — because what is under test is that CloudFormation publishes that request
//! itself rather than while serving a client's next read.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::provisioning::custom_resource;
use smithy_server_runtime::MultiServiceRouter;

use super::custom_resource_providers::{
    awaited_request_of_type, emulator, provider_token, put_response,
    settled_with_a_responsive_provider, POLL_ATTEMPTS, POLL_INTERVAL,
};
use super::harness::{call, create_stack, member, xml_text};
use super::stack_configuration_and_provider_limits::response_of_size;
use super::stack_lifecycle::{resource_event_reason, resource_event_statuses};

/// The resource whose provider answers inside the bound, which the failing one depends on.
const FITS: &str = "Fits";

/// The resource whose provider answers with the document each row is about.
const OVERFLOWS: &str = "Overflows";

/// The `PhysicalResourceId` the provider mints for [`FITS`].
const FITS_ID: &str = "fits-instance";

/// The `PhysicalResourceId` the provider names in the documents below. A row whose document
/// CloudFormation READ is known by it from then on; a row whose document is past the bound is
/// not read at all, so this id never reaches CloudFormation.
const PROVIDER_ID: &str = "overflows-instance";

/// The `Reason` the one answer inside the bound states.
const STATED_REASON: &str = "the provisioner could not reach its backend";

/// The `ServiceTimeout` both resources declare: a value inside the documented 1-14400 range,
/// so neither resource is refused before its provider is asked.
const SERVICE_TIMEOUT: &str = "120";

/// What a resource whose provider answered with a document past the bound reports as its
/// failure reason: entirely fixed text, spelled out here rather than read off the service's own
/// constant, so this suite pins the WORDING as well as where it is reported.
const RESPONSE_TOO_LONG_REASON: &str = "Response object is too long.";

/// How many hexadecimal characters the unique segment of an identifier CloudFormation assigns
/// carries — the same figure the suites that read a generated physical id hold it to
/// (`stack_output_resolution`), and the digest is upper-case hexadecimal.
const ASSIGNED_ID_HEX_DIGITS: usize = 12;

/// How long the poll that makes NO CloudFormation call waits for the request CloudFormation
/// owes, and how often it looks.
///
/// Generous on purpose: the window is the one the emulator drives a landed answer's work in,
/// and the test waits for the request to APPEAR rather than for any amount of time to pass.
const DRIVEN_POLL_INTERVAL: Duration = Duration::from_millis(50);
const DRIVEN_POLL_ATTEMPTS: usize = 400;

/// One row: the document the provider of [`OVERFLOWS`] PUTs, and what the failure it causes
/// must then look like.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The document answered, built per row so a row states its size rather than spelling the
    /// padding out.
    answer: fn() -> Value,
    /// Whether that document is one CloudFormation READ: a document inside the bound is, and
    /// one past it is not.
    read: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a SUCCESS document one byte past the bound",
        stack: "driven-over-by-one",
        answer: || {
            response_of_size(
                custom_resource::MAX_RESPONSE_BODY_BYTES + 1,
                custom_resource::SUCCESS,
                PROVIDER_ID,
            )
        },
        read: false,
    },
    Row {
        case: "a SUCCESS document well past the bound",
        stack: "driven-over-by-far",
        answer: || {
            response_of_size(
                custom_resource::MAX_RESPONSE_BODY_BYTES * 4,
                custom_resource::SUCCESS,
                PROVIDER_ID,
            )
        },
        read: false,
    },
    Row {
        case: "a FAILED document one byte past the bound",
        stack: "driven-over-failed",
        answer: || {
            response_of_size(
                custom_resource::MAX_RESPONSE_BODY_BYTES + 1,
                custom_resource::FAILED,
                PROVIDER_ID,
            )
        },
        read: false,
    },
    Row {
        case: "a FAILED document inside the bound",
        stack: "driven-inside-bound",
        answer: || {
            json!({
                "Status": custom_resource::FAILED,
                "Reason": STATED_REASON,
                "PhysicalResourceId": PROVIDER_ID,
            })
        },
        read: true,
    },
];

/// The template every row deploys: two custom resources of the same provider, the second
/// stating `DependsOn` the first — so the order the rollback has to take the two down in is
/// the reverse of the order it created them in.
fn template() -> String {
    let declaration = || {
        json!({
            "Type": "Custom::Sized",
            "Properties": {
                custom_resource::SERVICE_TOKEN: provider_token(),
                custom_resource::SERVICE_TIMEOUT: SERVICE_TIMEOUT,
            },
        })
    };
    let mut overflows = declaration();
    overflows["DependsOn"] = json!(FITS);
    json!({ "Resources": { FITS: declaration(), OVERFLOWS: overflows } }).to_string()
}

/// Every row: the answer fails the resource, and CloudFormation itself publishes the `Delete`
/// that failure owes — before any `Delete` for the resource the failed one depends on, under
/// the id CloudFormation has for it — and the stack settles at `ROLLBACK_COMPLETE` with one
/// `CREATE_FAILED` event carrying the reason the answer earned.
#[tokio::test]
async fn an_answer_that_fails_a_resource_drives_its_rollback_with_no_further_client_call() {
    for row in ROWS {
        let case = row.case;
        let (router, engine) = emulator(case).await;
        let (status_code, created) = create_stack(&router, row.stack, &template(), &[]).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected CreateStack to be accepted, got {status_code} with {created}"
        );

        // The resource the failing one depends on is created first, by an answer of exactly the
        // size the protocol allows — the bound itself, which is read.
        let (stack_id, fits_request, _) = awaited_create(&router, &engine, case, row.stack, FITS)
            .await;
        let (answered, answer_body) = put_response(
            &router,
            &engine,
            &stack_id,
            FITS,
            &fits_request.request_id,
            &response_of_size(
                custom_resource::MAX_RESPONSE_BODY_BYTES,
                custom_resource::SUCCESS,
                FITS_ID,
            ),
        )
        .await;
        assert_eq!(
            answered, 200,
            "case {case:?}: expected the provider's answer to be accepted, got {answered} with \
             {answer_body}"
        );
        let settled = resource_status(&router, case, row.stack, FITS, "CREATE_COMPLETE").await;
        assert_eq!(
            settled, "CREATE_COMPLETE",
            "case {case:?}: a document of exactly the bound must complete {FITS}, got {settled:?}"
        );

        // ...and then the resource this row is about is answered with the row's document.
        let (_, request, assigned) =
            awaited_create(&router, &engine, case, row.stack, OVERFLOWS).await;
        assert!(
            is_assigned_id(&assigned, row.stack, OVERFLOWS),
            "case {case:?}: CloudFormation's own id for {OVERFLOWS} must be \
             <stack name>-<logical id>-<unique id>, got {assigned:?}"
        );
        let (answered, answer_body) = put_response(
            &router,
            &engine,
            &stack_id,
            OVERFLOWS,
            &request.request_id,
            &(row.answer)(),
        )
        .await;
        assert_eq!(
            answered, 200,
            "case {case:?}: a provider's write is accepted either way, got {answered} with \
             {answer_body}"
        );

        // From here to the assertions below, NOT ONE CloudFormation operation is called: the
        // `Delete` the failure owes has to be published by CloudFormation itself.
        let delete = driven_delete(&engine, case, row.stack, OVERFLOWS).await;
        let expected_id = if row.read { PROVIDER_ID } else { assigned.as_str() };
        assert_eq!(
            delete.physical_resource_id.as_deref(),
            Some(expected_id),
            "case {case:?}: the failure-path Delete must name {expected_id:?}, got {delete:?}"
        );
        // ...and the resource the failed one depends on is not being deleted yet: a rollback
        // takes a resource down before the ones it depends on.
        let dependency = awaited_request_of_type(
            &engine,
            row.stack,
            FITS,
            custom_resource::request_type::DELETE,
        );
        assert!(
            dependency.is_none(),
            "case {case:?}: no Delete for {FITS} may be pending while {OVERFLOWS} is still \
             being deleted, got {dependency:?}"
        );

        // The rollback is carried through by a provider that answers whatever it is sent, which
        // is what then takes the dependency down too.
        let (status, body) = settled_with_a_responsive_provider(
            &router,
            &engine,
            case,
            row.stack,
            PROVIDER_ID,
        )
        .await;
        assert_eq!(
            status, "ROLLBACK_COMPLETE",
            "case {case:?}: the failed resource must roll the stack back, got {status} with \
             {body}"
        );
        let dependency_statuses = resource_event_statuses(&router, case, row.stack, FITS).await;
        assert!(
            dependency_statuses.iter().any(|status| status.starts_with("DELETE_")),
            "case {case:?}: the rollback must get to {FITS} as well, got \
             {dependency_statuses:?}"
        );

        // The failed resource records exactly ONE failure of its create, with the reason the
        // answer earned: CloudFormation's own diagnostic for a document it did not read, and
        // the response it received for one it did.
        let failures = resource_event_statuses(&router, case, row.stack, OVERFLOWS)
            .await
            .into_iter()
            .filter(|status| status == "CREATE_FAILED")
            .count();
        assert_eq!(
            failures, 1,
            "case {case:?}: expected exactly one {OVERFLOWS} CREATE_FAILED event, got {failures}"
        );
        let expected_reason = if row.read {
            // The sentence CloudFormation writes for an answer it DID read, spelled out here
            // too: its fixed text, with the message the answer stated and the request it
            // answered as its only variable parts, both taken from the exchange.
            format!(
                "Received response status [{}] from custom resource. Message returned: \
                 {STATED_REASON} (RequestId: {})",
                custom_resource::FAILED, request.request_id
            )
        } else {
            RESPONSE_TOO_LONG_REASON.to_string()
        };
        let reason = resource_event_reason(&router, row.stack, OVERFLOWS, "CREATE_FAILED").await;
        assert_eq!(
            reason, expected_reason,
            "case {case:?}: expected the failure to read {expected_reason:?}, got {reason:?}"
        );
    }
}

/// Whether `id` is the identifier CLOUDFORMATION assigns a resource the template does not name:
/// `<stack name>-<logical id>-<unique id>`, the unique segment being the upper-case digest
/// `state::template` derives from the stack and the logical id. Checked as a FORM, since no
/// request fixes those characters.
fn is_assigned_id(id: &str, stack_name: &str, logical_id: &str) -> bool {
    id.strip_prefix(&format!("{stack_name}-{logical_id}-"))
        .is_some_and(|unique| {
            unique.len() == ASSIGNED_ID_HEX_DIGITS
                && unique.chars().all(|character| {
                    character.is_ascii_hexdigit() && !character.is_ascii_lowercase()
                })
        })
}

/// The `Create` request CloudFormation is waiting for an answer to about `logical_id`, polled
/// for: a resource is asked about as the deployment walk reaches it.
async fn awaited_create(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack_name: &str,
    logical_id: &str,
) -> (String, custom_resource::ProviderRequest, String) {
    for _ in 0..POLL_ATTEMPTS {
        if let Some(awaited) = awaited_request_of_type(
            engine,
            stack_name,
            logical_id,
            custom_resource::request_type::CREATE,
        ) {
            return awaited;
        }
        let (status_code, body) =
            call(router, "DescribeStacks", &[member("StackName", stack_name)]).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected DescribeStacks to answer, got {status_code} with {body}"
        );
        assert!(
            xml_text(&body, "StackStatus")
                .as_deref()
                .is_some_and(|status| status.ends_with("_IN_PROGRESS")),
            "case {case:?}: the stack concluded without ever asking about {logical_id}: {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: {logical_id} was never sent a Create request");
}

/// The `ResourceStatus` `DescribeStackResource` reports for `logical_id`, polled until it
/// reaches `expected` — a stack's own transitions are clock-owed, so a read is what makes them
/// due. The last status read is answered either way, for the assertion to report.
async fn resource_status(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    logical_id: &str,
    expected: &str,
) -> String {
    let mut last = String::new();
    for _ in 0..POLL_ATTEMPTS {
        let (status_code, body) = call(
            router,
            "DescribeStackResource",
            &[
                member("StackName", stack_name),
                member("LogicalResourceId", logical_id),
            ],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected DescribeStackResource to answer, got {status_code} with \
             {body}"
        );
        last = xml_text(&body, "ResourceStatus").unwrap_or_default();
        if last == expected {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    last
}

/// The `Delete` CloudFormation published about `logical_id` on its OWN, waited for without
/// calling one CloudFormation operation.
///
/// The wait is for the request to appear, not for an amount of time to pass: the deadline is
/// far longer than the window the emulator drives a landed answer's work in, so a request that
/// is ever published is seen.
async fn driven_delete(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack_name: &str,
    logical_id: &str,
) -> custom_resource::ProviderRequest {
    for _ in 0..DRIVEN_POLL_ATTEMPTS {
        if let Some((_, request, _)) = awaited_request_of_type(
            engine,
            stack_name,
            logical_id,
            custom_resource::request_type::DELETE,
        ) {
            return request;
        }
        tokio::time::sleep(DRIVEN_POLL_INTERVAL).await;
    }
    panic!(
        "case {case:?}: no Delete for {logical_id} of {stack_name:?} was ever published without \
         a CloudFormation call"
    );
}

