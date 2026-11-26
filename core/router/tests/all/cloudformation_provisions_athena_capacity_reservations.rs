// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::Athena::CapacityReservation` really reserves
//! the capacity in Amazon Athena, and the handle the stack publishes addresses the reservation
//! that service holds.
//!
//! The rule under test: **a resource of a type this emulator provisions is created in the
//! service that OWNS it, through that service's own public API and with every declared property
//! applied — so the identifier a stack publishes for it resolves in that service's own read
//! right after the stack completes, a changed declaration is applied to the resource that
//! exists, and deleting the stack releases it there.** For this type the identifier a stack
//! publishes is the reservation's ARN — the resource type's own primary identifier, which is
//! what its `Ref` returns — whose last segment is the NAME every Athena call on one takes:
//! `GetCapacityReservation`, `UpdateCapacityReservation`, `CancelCapacityReservation` and
//! `DeleteCapacityReservation` each address a reservation by it, and the workgroups that run on
//! the reservation are assigned to that same name. Two crates have to agree for that to hold
//! (CloudFormation makes the calls, Athena serves them), so the suite lives here: a crate-local
//! test can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: whether the template NAMES the
//! reservation or leaves the name to CloudFormation, whether it is written in JSON or YAML, how
//! much capacity it requests, whether it assigns a workgroup to the reservation, and the Region
//! the stack is deployed in.
//!
//! The UPDATE half and the DELETE half are rows of the same rule: a changed `TargetDpus` is
//! applied to the reservation Athena already holds, and deleting the stack takes it out of the
//! account — which Athena does in two steps, a reservation having to be cancelled before it can
//! be deleted. The REFUSAL half is one more: a declaration Athena refuses — capacity past the
//! account's Provisioned DPU quota — leaves the stack rolled back with nothing reserved.
//!
//! No expected value is read out of the answer being checked: each is either what the row's own
//! template declares, or what the OWNING service reports for the resource.
//!
//! The awsQuery form encoder and the XML element readers are this test crate's own
//! ([`crate::query_wire`]), not another copy.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{form_encode, opt_element, outputs_of};

/// How long one `DescribeStacks` poll waits before the next, and how many are made.
///
/// A stack's own status transitions and the allocation and cancellation Athena owes on their
/// clocks are what the suite waits for, so it waits for the STATUS rather than for a span, with
/// a deadline generous enough for a loaded host.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLLS: usize = 600;

/// The `x-amz-target` prefix of Amazon Athena: the local name of its Smithy service shape
/// (`com.amazonaws.athena#AmazonAthena`), which is how the awsJson protocols build that header.
const ATHENA_TARGET_PREFIX: &str = "AmazonAthena";

/// The status a reservation holding the capacity it asked for is in, which is the status
/// CloudFormation waits for before it reports the resource created.
const ACTIVE: &str = "ACTIVE";

/// The account id every ARN this emulator mints carries, which is the account whose Athena
/// capacity these stacks reserve.
const ACCOUNT: &str = "000000000000";

/// The longest a capacity reservation name may be (`CapacityReservationName`,
/// `^[a-zA-Z0-9._-]+$` and `@length` 1..128 in Athena's own model), which a name CloudFormation
/// GENERATES is held to.
const MAX_RESERVATION_NAME: usize = 128;

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other suite
/// here; the in-process callback CloudFormation makes signs its request either way (the
/// credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_athena_amazonathena::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which is
/// what routes the request and what each handler reads its Region from. The signature is never
/// checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire, signed for `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    members: &[(&str, &str)],
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
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one Amazon Athena operation over the awsJson1_1 wire its model declares, signed for
/// `region`.
async fn athena(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("athena.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{ATHENA_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("athena", region))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, payload)
}

/// Wait for `stack` to report `wanted`, looking again until it does or the deadline passes. A
/// stack that reaches another TERMINAL status fails the suite at once, quoting the answer: the
/// reason a resource failed is in it.
async fn wait_for_stack_status(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
    wanted: &str,
) -> String {
    let mut body = String::new();
    for _ in 0..POLLS {
        let (status, reported) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {reported}"
        );
        body = reported;
        let reported_status = opt_element(&body, "StackStatus").unwrap_or_default();
        if reported_status == wanted {
            return body;
        }
        assert!(
            !reported_status.ends_with("_FAILED") && !reported_status.starts_with("ROLLBACK"),
            "stack {stack:?}: expected {wanted:?}, got {reported_status:?} with answer {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("stack {stack:?} never reached {wanted:?}; last answer was {body}");
}

/// Wait for `stack` to reach any TERMINAL status, which is what a deployment that fails ends in.
async fn wait_for_terminal_status(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
) -> (String, String) {
    let mut body = String::new();
    for _ in 0..POLLS {
        let (status, reported) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {reported}"
        );
        body = reported;
        let reported_status = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported_status.ends_with("_IN_PROGRESS") {
            return (reported_status, body);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("stack {stack:?} never reached a terminal status; last answer was {body}");
}

/// One output of a `DescribeStacks` answer, or a panic naming the case and what was there.
fn output(outputs: &BTreeMap<String, String>, key: &str, case: &str) -> String {
    outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected an output {key:?}, got {outputs:?}"))
}

/// Wait for Athena to report the reservation `name` in `status`, and hand back the
/// `CapacityReservation` that reported it.
async fn wait_for_reservation(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    status: &str,
    case: &str,
) -> Value {
    let mut last = Value::Null;
    for _ in 0..POLLS {
        let (code, body) = athena(
            router,
            region,
            "GetCapacityReservation",
            &json!({ "Name": name }),
        )
        .await;
        last = body;
        if code == 200 && last["CapacityReservation"]["Status"] == json!(status) {
            return last["CapacityReservation"].clone();
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the reservation {name} never reached {status}; last read {last}");
}

/// One row of the table: a template, and what the rule says the reservation it declares must
/// then be in Athena.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region the reservation is reserved in.
    region: &'static str,
    /// The stack name the row deploys under, whose NAME is the first segment of any name
    /// CloudFormation generates for a resource the template does not name.
    stack: &'static str,
    /// The template body, in the language the row writes it in.
    template: &'static str,
    /// The template the UPDATE of that stack declares, asking for another amount of capacity.
    updated: &'static str,
    /// The reservation name the template declares, or `None` for a template naming none — whose
    /// reservation carries the name CloudFormation generated.
    declared_name: Option<&'static str>,
    /// The amount of capacity the first template requests, and the amount the update asks for.
    requested: i64,
    /// See [`Row::requested`].
    updated_to: i64,
    /// The workgroups the declaration assigns to the reservation, which Athena then reports as
    /// its capacity assignment configuration.
    ///
    /// The templates declare them under the property the TYPE publishes (`WorkgroupNames`),
    /// where Athena's own answer carries them under the member its model spells
    /// (`WorkGroupNames`) — the one name the two surfaces spell differently.
    assigned: &'static [&'static str],
}

const ROWS: &[Row] = &[
    Row {
        case: "a named reservation with an assigned workgroup, in JSON",
        region: "us-east-1",
        stack: "athena-capacity-json",
        template: r#"{
          "AWSTemplateFormatVersion": "2010-09-09",
          "Resources": {
            "Analytics": {
              "Type": "AWS::Athena::WorkGroup",
              "Properties": {"Name": "capacity-analytics"}
            },
            "Reserved": {
              "Type": "AWS::Athena::CapacityReservation",
              "DependsOn": "Analytics",
              "Properties": {
                "Name": "orders-capacity",
                "TargetDpus": 24,
                "Tags": [{"Key": "Owner", "Value": "analytics"}],
                "CapacityAssignmentConfiguration": {
                  "CapacityAssignments": [{"WorkgroupNames": ["capacity-analytics"]}]
                }
              }
            }
          },
          "Outputs": {
            "ReservationRef": {"Value": {"Ref": "Reserved"}},
            "ReservationArn": {"Value": {"Fn::GetAtt": ["Reserved", "Arn"]}}
          }
        }"#,
        updated: r#"{
          "AWSTemplateFormatVersion": "2010-09-09",
          "Resources": {
            "Analytics": {
              "Type": "AWS::Athena::WorkGroup",
              "Properties": {"Name": "capacity-analytics"}
            },
            "Reserved": {
              "Type": "AWS::Athena::CapacityReservation",
              "DependsOn": "Analytics",
              "Properties": {
                "Name": "orders-capacity",
                "TargetDpus": 48,
                "Tags": [{"Key": "Owner", "Value": "analytics"}],
                "CapacityAssignmentConfiguration": {
                  "CapacityAssignments": [{"WorkgroupNames": ["capacity-analytics"]}]
                }
              }
            }
          },
          "Outputs": {
            "ReservationRef": {"Value": {"Ref": "Reserved"}},
            "ReservationArn": {"Value": {"Fn::GetAtt": ["Reserved", "Arn"]}}
          }
        }"#,
        declared_name: Some("orders-capacity"),
        requested: 24,
        updated_to: 48,
        assigned: &["capacity-analytics"],
    },
    Row {
        case: "a reservation CloudFormation names, with no assignment, in YAML",
        region: "us-west-2",
        stack: "athena-capacity-yaml",
        template: concat!(
            "Resources:\n",
            "  Reserved:\n",
            "    Type: AWS::Athena::CapacityReservation\n",
            "    Properties:\n",
            "      TargetDpus: 4\n",
            "Outputs:\n",
            "  ReservationRef:\n",
            "    Value: !Ref Reserved\n",
            "  ReservationArn:\n",
            "    Value: !GetAtt Reserved.Arn\n",
        ),
        updated: concat!(
            "Resources:\n",
            "  Reserved:\n",
            "    Type: AWS::Athena::CapacityReservation\n",
            "    Properties:\n",
            "      TargetDpus: 12\n",
            "Outputs:\n",
            "  ReservationRef:\n",
            "    Value: !Ref Reserved\n",
            "  ReservationArn:\n",
            "    Value: !GetAtt Reserved.Arn\n",
        ),
        declared_name: None,
        requested: 4,
        updated_to: 12,
        assigned: &[],
    },
];

/// Every row: the name the stack publishes addresses a capacity reservation Athena really holds,
/// requesting the capacity the template declared and assigned to the workgroups it declared; a
/// changed declaration is applied to that same reservation; and deleting the stack takes it out
/// of the account.
#[tokio::test]
async fn a_stack_provisions_the_capacity_reservation_its_template_declares() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack = format!("{}-{index}", row.stack);

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[
                ("StackName", &stack),
                ("TemplateBody", row.template),
                ("Tags.member.1.Key", "Stack"),
                ("Tags.member.1.Value", "athena-capacity-suite"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );
        let described = wait_for_stack_status(&router, region, &stack, "CREATE_COMPLETE").await;
        let outputs = outputs_of(&described);
        // `Ref` on this type returns the reservation's ARN, which is the type's primary
        // identifier — so the name every Athena call takes is that ARN's last segment, and the
        // two Outputs (the `Ref` and the published `Arn`) carry the same value.
        let published = output(&outputs, "ReservationRef", case);
        let name = published
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();
        if let Some(declared) = row.declared_name {
            assert_eq!(
                name, declared,
                "case {case:?}: expected the reservation to carry the declared name \
                 {declared:?}, got {published:?}"
            );
        }
        // A name CloudFormation generated is still a name Athena's own rule admits.
        assert!(
            !name.is_empty()
                && name.len() <= MAX_RESERVATION_NAME
                && name.chars().all(|character| {
                    character.is_ascii_alphanumeric()
                        || character == '.'
                        || character == '_'
                        || character == '-'
                }),
            "case {case:?}: expected a name inside Athena's own CapacityReservationName rule, \
             got {name:?}"
        );
        // The ARN the stack publishes for the reservation is the one Athena's own resource ARNs
        // carry for that name (`arn:${Partition}:athena:${Region}:${Account}:<type>/<name>`, the
        // "Resource types defined by Amazon Athena" table of the IAM Service Authorization
        // Reference), so a policy or a tag read built from it addresses the reservation that
        // service holds — and the `Ref` resolves to exactly that ARN rather than to the bare
        // name.
        let expected_arn = format!("arn:aws:athena:{region}:{ACCOUNT}:capacity-reservation/{name}");
        assert_eq!(
            output(&outputs, "ReservationArn", case),
            expected_arn,
            "case {case:?}: expected the reservation's own ARN, got {outputs:?}"
        );
        assert_eq!(
            published, expected_arn,
            "case {case:?}: expected a Ref to this resource to resolve to its ARN, got \
             {published:?}"
        );

        // Athena itself resolves the published name to the reservation the template declared,
        // and the stack was held open until that service reported the capacity allocated.
        let reservation = wait_for_reservation(&router, region, &name, ACTIVE, case).await;
        assert_eq!(
            reservation["Name"].as_str(),
            Some(name.as_str()),
            "case {case:?}: the read must report the reservation addressed, got {reservation}"
        );
        assert_eq!(
            reservation["TargetDpus"].as_i64(),
            Some(row.requested),
            "case {case:?}: the reservation must request what the template declared, got \
             {reservation}"
        );
        assert_eq!(
            reservation["AllocatedDpus"].as_i64(),
            Some(row.requested),
            "case {case:?}: an active reservation holds the capacity it requested, got \
             {reservation}"
        );

        // The workgroups the declaration assigned run on the reservation, read through Athena's
        // own projection of that configuration.
        let (status, assigned) = athena(
            &router,
            region,
            "GetCapacityAssignmentConfiguration",
            &json!({ "CapacityReservationName": name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the assignment configuration must be readable, got {status} with \
             body {assigned}"
        );
        let reported: Vec<String> = assigned["CapacityAssignmentConfiguration"]
            ["CapacityAssignments"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .flat_map(|assignment| {
                assignment["WorkGroupNames"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            })
            .filter_map(|work_group| work_group.as_str().map(ToString::to_string))
            .collect();
        let expected: Vec<String> = row.assigned.iter().map(ToString::to_string).collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the declared workgroups on the reservation, got {assigned}"
        );

        // A CHANGED declaration is applied to the reservation that exists: the amount it
        // requests is the new one, under the same name.
        let (status, body) = cloudformation(
            &router,
            region,
            "UpdateStack",
            &[("StackName", &stack), ("TemplateBody", row.updated)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body}"
        );
        let described = wait_for_stack_status(&router, region, &stack, "UPDATE_COMPLETE").await;
        let updated_ref = output(&outputs_of(&described), "ReservationRef", case);
        assert_eq!(
            updated_ref, published,
            "case {case:?}: a changed amount must not replace the reservation, got \
             {updated_ref:?}"
        );
        let reservation = wait_for_reservation(&router, region, &name, ACTIVE, case).await;
        assert_eq!(
            reservation["TargetDpus"].as_i64(),
            Some(row.updated_to),
            "case {case:?}: the reservation must request what the update declared, got \
             {reservation}"
        );

        // Deleting the stack releases what Athena holds for it — cancelled, then deleted, the
        // two steps that service requires — so the read of the name Athena serves afterwards
        // is the refusal of a reservation it no longer holds. The removal is POLLED for
        // (CloudFormation holds the stack's deletion open until Athena reports the reservation
        // gone), and what is polled is the owning service's own read: a deleted stack is no
        // longer addressable by NAME at all.
        let (status, body) =
            cloudformation(&router, region, "DeleteStack", &[("StackName", &stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
        );
        let mut gone = Value::Null;
        let mut released = false;
        for _ in 0..POLLS {
            let (status, read) = athena(
                &router,
                region,
                "GetCapacityReservation",
                &json!({ "Name": name }),
            )
            .await;
            gone = read;
            if status == 400 {
                released = true;
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        assert!(
            released,
            "case {case:?}: the stack's deletion must take the reservation out of the account, \
             last read {gone}"
        );
        assert_eq!(
            gone.get("__type").and_then(Value::as_str),
            Some("InvalidRequestException"),
            "case {case:?}: expected InvalidRequestException for a reservation Athena no longer \
             holds, got {gone}"
        );
    }
}

/// A declaration Athena REFUSES does not report a resource nothing created: the stack rolls back
/// with nothing reserved, and the name it declared resolves to nothing afterwards.
///
/// The rows differ along WHICH rule Athena refuses the declaration for: capacity past the
/// account's own Provisioned DPU quota, an amount that is not a whole block of capacity, and a
/// name the account already holds a reservation under.
#[tokio::test]
async fn a_capacity_declaration_athena_refuses_fails_the_stack() {
    let router = build_router();
    let region = "us-east-2";

    // The reservation a client already holds, which the third row's template re-declares.
    let (status, created) = athena(
        &router,
        region,
        "CreateCapacityReservation",
        &json!({ "Name": "already-reserved", "TargetDpus": 4 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the client's own CreateCapacityReservation to succeed, got {status} with body \
         {created}"
    );

    // (case, the template, the name it declares, the status a read of that name must answer
    // once the stack has rolled back). The last row's name is the CLIENT's own reservation,
    // which a stack that never created it must leave exactly where it is.
    let rows: &[(&str, &str, &str, u16)] = &[
        (
            "capacity past the account's Provisioned DPU quota",
            r#"{"Resources": {"TooMuch": {"Type": "AWS::Athena::CapacityReservation",
               "Properties": {"Name": "past-the-quota", "TargetDpus": 1000000}}}}"#,
            "past-the-quota",
            400,
        ),
        (
            "an amount that is not a whole block of capacity",
            r#"{"Resources": {"Partial": {"Type": "AWS::Athena::CapacityReservation",
               "Properties": {"Name": "part-of-a-block", "TargetDpus": 6}}}}"#,
            "part-of-a-block",
            400,
        ),
        (
            "a name the account already holds a reservation under",
            r#"{"Resources": {"Clash": {"Type": "AWS::Athena::CapacityReservation",
               "Properties": {"Name": "already-reserved", "TargetDpus": 8}}}}"#,
            "already-reserved",
            200,
        ),
    ];

    for (index, (case, template, name, expected_status)) in rows.iter().enumerate() {
        let stack = format!("athena-capacity-refusal-{index}");
        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[("StackName", &stack), ("TemplateBody", template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );
        let (reported, described) = wait_for_terminal_status(&router, region, &stack).await;
        assert_eq!(
            reported, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the stack to roll back on a declaration Athena refuses, \
             got {reported:?} with answer {described}"
        );
        let (status, read) = athena(
            &router,
            region,
            "GetCapacityReservation",
            &json!({ "Name": name }),
        )
        .await;
        assert_eq!(
            status, *expected_status,
            "case {case:?}: expected HTTP {expected_status} from a read of {name:?} after the \
             rollback, got {status} with body {read}"
        );
    }
}
