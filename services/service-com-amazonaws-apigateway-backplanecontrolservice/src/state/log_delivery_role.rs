// SPDX-License-Identifier: CC0-1.0

//! Which CloudWatch Logs call API Gateway may make for a stage's logs, decided over the
//! policies in force on the ACCOUNT's CloudWatch role.
//!
//! `Account$cloudwatchRoleArn` is "the ARN of an Amazon CloudWatch role for the current
//! Account" — the role API Gateway ASSUMES to write the account's logs — so every
//! CloudWatch Logs call this service makes for a stage (creating the group it publishes,
//! opening the stream, putting the events) is a call made by that role rather than by
//! nobody. A call made by a principal is an IAM-authorized call, so a policy in force on
//! the role that says NO about it is the boundary on where API Gateway may write: the call
//! is not made, and what it would have written is not there.
//!
//! The decision is the tree's one evaluation of policy documents
//! ([`::service_common::iam_authorization`]) over the identity-based documents IAM holds for
//! the role, taken against the ARN of the resource the call addresses: the log GROUP for the
//! create of a group ([`group_creation_denied`]), and the log STREAM the records go into for
//! the stream and event writes ([`record_delivery_denied`]) — the resources IAM publishes
//! those actions for ("Actions defined by Amazon CloudWatch Logs", IAM Service Authorization
//! Reference), so a document scoped to a group's ARN with everything under it covers the
//! writes into it and a document naming the group alone covers its creation.
//!
//! # Only an explicit `Deny` withholds a delivery
//!
//! An EXPLICIT `Deny` is an answer about this role: a statement that says this write may not
//! be made, which IAM lets no grant overturn. Everything else is not an answer about what the
//! role may do, so it withholds nothing — the same fail-open reading every other gate over a
//! role's documents in this tree keeps:
//!
//! * an engine carrying no IAM store, where no role can exist, and a role that store holds no
//!   record for (a push role is routinely provisioned out of band);
//! * a role carrying no identity-based policy at all, or one whose documents merely say
//!   nothing about the write — the routine state of a role created for a background job;
//! * an account naming no role, which is a stage that logs nothing at all and never reaches
//!   here ([`crate::service::invoke::stage_logs`]).
//!
//! An attachment whose document this account does not hold — an entry of AWS's own catalog,
//! such as the managed policy AWS publishes for this very delivery — is read for what it is:
//! a grant nothing can read, which can neither state the `Deny` this looks for nor overturn
//! one a readable document states.
//!
//! # Why the records are read here rather than asked for
//!
//! `service_common::iam_role::role_denies_action` answers this very question for a service
//! that does NOT hold the IAM store, by asking IAM to simulate the role's policies over the
//! in-process transport; its own rule for which route to take is that "a service with the
//! records at hand reads them, and every other asks here". This service has them at hand — it
//! already decides an `AWS_IAM`-authorized invoke out of the same two accessors
//! ([`crate::service::invoke::execute_api_authorization`]) — so the records are read, and the
//! two routes cannot disagree because both end in the one evaluation of a policy document
//! ([`::service_common::iam_authorization`]), which the simulation is itself built on. Reading
//! them keeps a delivery decision synchronous and free of a per-request hop into another
//! service, which is what lets it be taken for every request a traced stage answers.
//!
//! LOCK ORDER: the IAM store's lock is taken and released inside this call, and no guard of
//! this service's store is taken at all — so a caller holding no guard (which is how the
//! delivery paths reach this, after their own read of the stage has been released) can never
//! close a cycle through it.

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_common::iam_authorization::{self as iam, Decision};

/// The action that brings a log group into existence, which is what a group API Gateway
/// publishes under its own name needs before anything is written into it.
///
/// Source: the action IAM publishes for `CreateLogGroup` ("Actions defined by Amazon
/// CloudWatch Logs", IAM Service Authorization Reference), written with CloudWatch Logs'
/// own action prefix ([`LOGS_ACTION_PREFIX`]).
const CREATE_LOG_GROUP: &str = "CreateLogGroup";

/// The two actions one DELIVERY of records performs, in the order they happen: the stream the
/// records go into is opened, and the records are put into it as log events. Source: the
/// actions IAM publishes for `CreateLogStream` and `PutLogEvents`, as above.
const DELIVERY_ACTIONS: &[&str] = &["CreateLogStream", "PutLogEvents"];

/// The action prefix CloudWatch Logs publishes its actions under.
const LOGS_ACTION_PREFIX: &str = "logs";

/// The vendor field of a CloudWatch Logs ARN, which is the service the log groups API
/// Gateway writes into belong to.
const LOGS_ARN_SERVICE: &str = "logs";

/// Whether the policies in force on `role_arn` explicitly DENY the creation of the log group
/// `log_group` in `region`.
///
/// `role_arn` is the account's CloudWatch role, the principal API Gateway makes the create
/// as. `true` means the create is not API Gateway's to make, so the group never comes into
/// existence; `false` for everything that is not such an answer (see the module docs).
pub fn group_creation_denied<Eng: Send + Sync + 'static>(
    engine: &Eng,
    role_arn: &str,
    region: &str,
    log_group: &str,
) -> bool {
    denies(
        engine,
        role_arn,
        &[CREATE_LOG_GROUP],
        &log_group_arn(region, log_group),
    )
}

/// Whether the policies in force on `role_arn` explicitly DENY the delivery of records into
/// the stream `log_stream` of the log group `log_group` in `region`.
///
/// Either of the two actions a delivery performs ([`DELIVERY_ACTIONS`]) being denied is a
/// delivery API Gateway cannot make, so the records are lost and the group stays as it was.
pub fn record_delivery_denied<Eng: Send + Sync + 'static>(
    engine: &Eng,
    role_arn: &str,
    region: &str,
    log_group: &str,
    log_stream: &str,
) -> bool {
    denies(
        engine,
        role_arn,
        DELIVERY_ACTIONS,
        &log_stream_arn(region, log_group, log_stream),
    )
}

/// The ARN of the log group `name`: the resource a create of that group addresses.
fn log_group_arn(region: &str, name: &str) -> String {
    ::emulator_server::emulator_arn(LOGS_ARN_SERVICE, region, &format!("log-group:{name}"))
}

/// The ARN of the stream `stream` of the log group `group`: the resource the stream and
/// event writes of a delivery address, which a document scoped to the group together with
/// everything under it covers.
fn log_stream_arn(region: &str, group: &str, stream: &str) -> String {
    ::emulator_server::emulator_arn(
        LOGS_ARN_SERVICE,
        region,
        &format!("log-group:{group}:log-stream:{stream}"),
    )
}

/// Whether the policies IAM holds for `role_arn` explicitly `Deny` ANY of `operations` on
/// `resource`.
fn denies<Eng: Send + Sync + 'static>(
    engine: &Eng,
    role_arn: &str,
    operations: &[&str],
    resource: &str,
) -> bool {
    documents_in_force(engine, role_arn)
        .is_some_and(|documents| denied_by(&documents, operations, resource))
}

/// Whether `documents` explicitly `Deny` ANY of `operations` on `resource` — the decision
/// itself, over the documents already read.
///
/// `operations` are CloudWatch Logs operation names; the action text IAM judges is composed
/// with that service's one action prefix here, so no call site spells one. The documents are
/// decided TOGETHER, as the policies in force on one principal are.
fn denied_by(documents: &[String], operations: &[&str], resource: &str) -> bool {
    operations.iter().any(|operation| {
        let action = format!("{LOGS_ACTION_PREFIX}:{operation}");
        iam::identity_decision(documents, &iam::Request::new(&action, resource, &[]))
            == Decision::Denied
    })
}

/// The policies in force on `role_arn`, as this account's IAM holds them, or `None` where
/// there is no answer about the role at all (see the module docs).
///
/// The role's identity-based documents and the PERMISSIONS BOUNDARY attached to it: "a
/// permissions boundary sets the maximum permissions that an identity-based policy can grant"
/// (IAM, permissions boundaries), and a `Deny` stated in one is an explicit deny like any
/// other, so the boundary is decided beside them. A boundary that merely does not ALLOW the
/// call is not read here: that is a grant this does not have rather than an answer that says
/// no, which is the fail-open reading the module docs state.
///
/// Owned values, so the IAM store's guard is released before anything is decided.
fn documents_in_force<Eng: Send + Sync + 'static>(
    engine: &Eng,
    role_arn: &str,
) -> Option<Vec<String>> {
    let state = iam_store::service_state(engine)?;
    let store = state.lock();
    // The role as THIS account holds it: the ARN the account holds it under carries its path,
    // and that is the ARN its policies are held against.
    let held_arn = store
        .roles
        .values()
        .find(|role| role.arn == role_arn)
        .map(|role| role.arn.clone())?;
    // An attachment this emulator resolved no document for is left out rather than refused
    // on: it can neither state a `Deny` nor overturn one (see the module docs).
    let mut documents = store.principal_policies(&held_arn)?.documents;
    documents.extend(store.permissions_boundary_document(&held_arn));
    Some(documents)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Region every ARN below names.
    const REGION: &str = "us-east-1";

    /// A document allowing every CloudWatch Logs action everywhere, which is the grant a push
    /// role carries.
    const BLANKET_ALLOW: &str =
        r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"logs:*","Resource":"*"}]}"#;

    /// A `Deny` of `actions` on `resources`, as a client scopes a push role down.
    fn deny(actions: &str, resources: &str) -> String {
        format!(
            r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Deny","Action":{actions},"Resource":{resources}}}]}}"#
        )
    }

    /// Whether `documents` withhold the creation of the group `group`: the decision
    /// [`group_creation_denied`] takes once the documents are read, called rather than
    /// restated — so the action text, the actions and the ARN under test are the ones the
    /// public reading uses.
    fn creation_withheld(documents: &[String], group: &str) -> bool {
        denied_by(
            documents,
            &[CREATE_LOG_GROUP],
            &log_group_arn(REGION, group),
        )
    }

    /// Whether `documents` withhold a delivery into the stream `stream` of `group`: the
    /// decision [`record_delivery_denied`] takes, called for the reason
    /// [`creation_withheld`] gives.
    fn delivery_withheld(documents: &[String], group: &str, stream: &str) -> bool {
        denied_by(
            documents,
            DELIVERY_ACTIONS,
            &log_stream_arn(REGION, group, stream),
        )
    }

    /// RULE: a `Deny` scoped to one log group withholds the call it names on THAT group and
    /// nothing else, whichever published form of the group's ARN the document is scoped by.
    #[test]
    fn a_deny_scoped_to_one_group_withholds_only_the_call_it_names_there() {
        let group = "API-Gateway-Execution-Logs_abc123/prod";
        let elsewhere = "API-Gateway-Execution-Logs_abc123/dev";
        let arn = log_group_arn(REGION, group);
        let under = format!("{arn}:*");
        // (case, the documents in force, creation withheld, delivery withheld)
        let rows: Vec<(&str, Vec<String>, bool, bool)> = vec![
            ("no document at all", Vec::new(), false, false),
            (
                "the push role's own grant and nothing else",
                vec![BLANKET_ALLOW.to_string()],
                false,
                false,
            ),
            (
                "the grant beside a deny of the create on the group and everything under it",
                vec![
                    BLANKET_ALLOW.to_string(),
                    deny(
                        r#""logs:CreateLogGroup""#,
                        &format!(r#"["{arn}","{under}"]"#),
                    ),
                ],
                true,
                false,
            ),
            (
                "a deny of the two writes a delivery makes, on everything under the group",
                vec![
                    BLANKET_ALLOW.to_string(),
                    deny(
                        r#"["logs:CreateLogStream","logs:PutLogEvents"]"#,
                        &format!(r#"["{arn}","{under}"]"#),
                    ),
                ],
                false,
                true,
            ),
            (
                "a deny of one of the two writes only: the delivery still cannot be made",
                vec![
                    BLANKET_ALLOW.to_string(),
                    deny(r#""logs:PutLogEvents""#, &format!(r#""{under}""#)),
                ],
                false,
                true,
            ),
            (
                "every logs action denied on ANOTHER group",
                vec![
                    BLANKET_ALLOW.to_string(),
                    deny(
                        r#""logs:*""#,
                        &format!(r#""{}:*""#, log_group_arn(REGION, elsewhere)),
                    ),
                ],
                false,
                false,
            ),
            (
                "another service's actions denied everywhere",
                vec![BLANKET_ALLOW.to_string(), deny(r#""s3:*""#, r#""*""#)],
                false,
                false,
            ),
        ];
        let stream = "2026-01-01/[prod]abc123";
        for (case, documents, creation, delivery) in rows {
            let answered = creation_withheld(&documents, group);
            assert_eq!(
                answered, creation,
                "case {case:?}: expected the group's creation withheld={creation}, got {answered}"
            );
            let answered = delivery_withheld(&documents, group, stream);
            assert_eq!(
                answered, delivery,
                "case {case:?}: expected the delivery withheld={delivery}, got {answered}"
            );
        }
    }

    /// RULE: the resources the two decisions are taken about are the ones IAM publishes those
    /// actions for — the group itself, and the stream the records go into.
    #[test]
    fn the_decisions_address_the_group_and_the_stream_the_records_go_into() {
        let group = "/apigw/access";
        let stream = "2026-01-01/[live]abc123";
        let arn = log_group_arn(REGION, group);
        assert_eq!(
            arn,
            format!(
                "arn:aws:logs:{REGION}:{account}:log-group:{group}",
                account = ::engine::emulator_account_id()
            ),
            "the log group ARN a create is judged against"
        );
        assert_eq!(
            log_stream_arn(REGION, group, stream),
            format!("{arn}:log-stream:{stream}"),
            "the log stream ARN a delivery is judged against"
        );
    }
}
