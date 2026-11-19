// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the three application-status-check operations this suite's issue
//! left scaffolded — `EnableApplicationStatusCheckSuppression`,
//! `DisassociateApplicationStatusCheck` and
//! `DescribeApplicationStatusCheckAssociations` — driven over the wire the service actually
//! speaks: `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rules under test are:
//!
//! 1. A SUPPRESSION IS PUT IN FORCE FOR THE INSTANCES THE REQUEST NAMES, and while it stands
//!    the instance-level application status `DescribeApplicationStatus` reports is
//!    `suppressed` whatever the checks report — the one reader of that state, so the enable
//!    and the describe cannot disagree. Lifting it through
//!    `DisableApplicationStatusCheckSuppression` reports the instance successful again.
//! 2. THE WHOLE BATCH IS RESOLVED BEFORE ANYTHING IS WRITTEN: a request naming one id this
//!    service cannot resolve suppresses nothing at all, and the enable's validator refuses
//!    every id it cannot resolve with ONE code, so a batch mixing a resolvable id with an
//!    unresolvable one is refused for the unresolvable one whichever order it named them in.
//! 3. A DISASSOCIATION REMOVES THE ASSOCIATION ITSELF, so the readers stop reporting it: a
//!    tag pair comes off the check `DescribeApplicationStatusChecks` reports and an instance
//!    stops holding the check's result, which is what `DescribeApplicationStatus` and
//!    `DescribeApplicationStatusCheckAssociations` read. An item that was not associated
//!    fails on its own row rather than refusing the items beside it.
//! 4. THE ASSOCIATIONS DESCRIBE REPORTS BOTH HALVES, one row per association, filtered by
//!    the one filter it documents and paged in a stable order.
//!
//! Each table row is a distinct input along what the rule generalises over — the kind of
//! target, the id form, the duration asked for, the filter value — so no row is the
//! specification.

use std::collections::BTreeSet;

use ec2::state::MAX_APPLICATION_STATUS_CHECK_TAG_ASSOCIATIONS;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query;
use super::wire::{child, child_text, document, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The image every launch here names.
const AMI: &str = "ami-0123456789abcdef0";

/// A well-formed instance id the account holds no instance for.
const UNKNOWN_INSTANCE: &str = "i-0123456789abcdef0";

/// A value that is no instance id at all.
const MALFORMED_INSTANCE: &str = "instance-one";

/// The `AssociationTypeEnum` member a tag association is reported under.
const ASSOCIATION_TYPE_TAG: &str = "tag";

/// The `AssociationTypeEnum` member an instance association is reported under.
const ASSOCIATION_TYPE_INSTANCE: &str = "instance-id";

/// One `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// The fields one call carries beside the action, as the harness takes them.
type Fields = Vec<(String, String)>;

/// One row of a refusal table: what the case is, the fields the call carries, and the status
/// and code it must be refused with.
type Refusal = (&'static str, Fields, u16, &'static str);

/// A refusal row that also varies WHICH check the call names.
type CheckRefusal = (&'static str, String, Fields, u16, &'static str);

/// The form fields of an `InstanceIds` list.
fn instance_fields(instance_ids: &[String]) -> Vec<(String, String)> {
    instance_ids
        .iter()
        .enumerate()
        .map(|(nth, id)| p(&format!("InstanceId.{}", nth + 1), id.clone()))
        .collect()
}

/// The form fields of a `TargetTagAssociations` list.
fn tag_fields(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .enumerate()
        .flat_map(|(nth, (key, value))| {
            let position = nth + 1;
            vec![
                p(&format!("TargetTagAssociation.{position}.Key"), *key),
                p(&format!("TargetTagAssociation.{position}.Value"), *value),
            ]
        })
        .collect()
}

/// One `CreateApplicationStatusCheck` call, as the id of the check it created.
async fn create_check(router: &MultiServiceRouter) -> String {
    let params = vec![p("Protocol", "http"), p("Port", "80")];
    let (status, body) =
        ec2_query::call(router, REGION, "CreateApplicationStatusCheck", &params).await;
    assert_eq!(
        status, 200,
        "CreateApplicationStatusCheck must succeed, got {status}: {body}"
    );
    ec2_query::element(&body, "applicationStatusCheckId")
        .unwrap_or_else(|| panic!("the create must report the id it minted: {body}"))
}

/// Launch one instance and answer its id.
async fn launch_one(router: &MultiServiceRouter) -> String {
    let params = vec![p("ImageId", AMI), p("MinCount", "1"), p("MaxCount", "1")];
    let (status, body) = ec2_query::call(router, REGION, "RunInstances", &params).await;
    assert_eq!(status, 200, "RunInstances must succeed, got {status}: {body}");
    ec2_query::elements(&body, "instanceId")
        .pop()
        .unwrap_or_else(|| panic!("RunInstances must report the instance it minted: {body}"))
}

/// The instance ids one response reports in `set`, beside the reason each row carries.
fn reported_rows(case: &str, body: &str, set: &str) -> Vec<(Option<String>, Option<String>)> {
    let parsed = document(case, body);
    set_items(parsed.root_element(), set)
        .into_iter()
        .map(|item| (child_text(item, "instanceId"), child_text(item, "reason")))
        .collect()
}

/// The instance-level application status `DescribeApplicationStatus` reports for one
/// instance — the one reader of what a suppression does.
///
/// Read by TREE off the row that carries an `<applicationStatus>`, the way the describe's own
/// suite reads it, so the status is never read out of a shape that merely shares the name.
async fn described_status(case: &str, router: &MultiServiceRouter, instance_id: &str) -> Option<String> {
    let (status, body) = ec2_query::call(
        router,
        REGION,
        "DescribeApplicationStatus",
        &[p("InstanceId.1", instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeApplicationStatus must succeed, got {status}: {body}"
    );
    let parsed = document(case, &body);
    let row = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "applicationStatus").is_some())?;
    child_text(child(row, "applicationStatus")?, "status")
}

// --- the suppression -------------------------------------------------------

/// Rule: a suppression put in force for the instances the request names is what the status
/// reader reports, and lifting it hands the instance back to its checks.
#[tokio::test]
async fn a_suppression_is_in_force_for_the_instances_it_named() {
    let case = "suppression round trip";
    let (router, _engine) = ec2_query::emulator();
    let check_id = create_check(&router).await;
    let first = launch_one(&router).await;
    let second = launch_one(&router).await;

    // A check has to be monitoring the instance for it to have an application status at all,
    // which is what the suppression suppresses.
    let mut associate = vec![p("ApplicationStatusCheckId", check_id.clone())];
    associate.extend(instance_fields(&[first.clone(), second.clone()]));
    let (status, body) =
        ec2_query::call(&router, REGION, "AssociateApplicationStatusCheck", &associate).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the association must succeed, got {status}: {body}"
    );

    let mut params = instance_fields(std::slice::from_ref(&first));
    params.push(p("DurationSeconds", "600"));
    let (status, body) = ec2_query::call(
        &router,
        REGION,
        "EnableApplicationStatusCheckSuppression",
        &params,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: enabling suppression must succeed, got {status}: {body}"
    );
    let successful = reported_rows(case, &body, "successfulResultSet");
    assert_eq!(
        successful,
        vec![(Some(first.clone()), None)],
        "case {case:?}: the instance it suppressed must be the one it reports, got \
         {successful:?}"
    );
    let parsed = document(case, &body);
    let suppressed_row = set_items(parsed.root_element(), "successfulResultSet")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: the answer must carry a successful row: {body}"));
    assert!(
        child(suppressed_row, "suppressAt").is_some(),
        "case {case:?}: a suppression reports when it started: {body}"
    );
    assert!(
        child(suppressed_row, "resumeAt").is_some(),
        "case {case:?}: a suppression with a duration reports when it ends: {body}"
    );

    let status_of_first = described_status(case, &router, &first).await;
    assert_eq!(
        status_of_first.as_deref(),
        Some("suppressed"),
        "case {case:?}: the suppressed instance must report that status, got \
         {status_of_first:?}"
    );
    let status_of_second = described_status(case, &router, &second).await;
    assert_ne!(
        status_of_second.as_deref(),
        Some("suppressed"),
        "case {case:?}: an instance nothing suppressed must not report that status, got \
         {status_of_second:?}"
    );

    // A second enable finds the suppression already in force: it has nothing to put in
    // force, so the instance is reported unsuccessful with the reason, and the suppression
    // the first call made still stands.
    let (status, body) = ec2_query::call(
        &router,
        REGION,
        "EnableApplicationStatusCheckSuppression",
        &instance_fields(std::slice::from_ref(&first)),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a repeated enable must answer, got {status}: {body}"
    );
    let unsuccessful = reported_rows(case, &body, "unsuccessfulResultSet");
    assert_eq!(
        unsuccessful.len(),
        1,
        "case {case:?}: the already-suppressed instance must be reported unsuccessful, got \
         {unsuccessful:?}"
    );
    assert_eq!(
        unsuccessful[0].0.as_deref(),
        Some(first.as_str()),
        "case {case:?}: the unsuccessful row must name the instance, got {unsuccessful:?}"
    );
    assert!(
        unsuccessful[0].1.is_some(),
        "case {case:?}: an unsuccessful row carries the reason, got {unsuccessful:?}"
    );
    let still = described_status(case, &router, &first).await;
    assert_eq!(
        still.as_deref(),
        Some("suppressed"),
        "case {case:?}: the first suppression must still stand, got {still:?}"
    );

    // Lifting it gives the instance back to its checks.
    let (status, body) = ec2_query::call(
        &router,
        REGION,
        "DisableApplicationStatusCheckSuppression",
        &instance_fields(std::slice::from_ref(&first)),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: disabling suppression must succeed, got {status}: {body}"
    );
    let lifted = described_status(case, &router, &first).await;
    assert_ne!(
        lifted.as_deref(),
        Some("suppressed"),
        "case {case:?}: a lifted suppression is no longer reported, got {lifted:?}"
    );
}

/// Rule: the enable resolves the WHOLE batch before it writes anything, and every id it
/// cannot resolve draws one code — so a batch mixing a resolvable id with an unresolvable
/// one is refused for the unresolvable one whichever order it named them in, and nothing is
/// suppressed.
#[tokio::test]
async fn an_unresolvable_id_anywhere_in_the_batch_refuses_the_whole_call() {
    let (router, _engine) = ec2_query::emulator();
    let known = launch_one(&router).await;

    // The rows differ in WHICH id cannot be resolved and WHERE in the batch it sits: a value
    // that is no instance id, a well-formed id the account holds nothing for, and the
    // all-zero placeholder, each before and after the resolvable id.
    let unresolvable = [MALFORMED_INSTANCE, UNKNOWN_INSTANCE, "i-00000000000000000"];
    for bad in unresolvable {
        for (label, batch) in [
            ("the unresolvable id first", vec![bad.to_string(), known.clone()]),
            ("the unresolvable id last", vec![known.clone(), bad.to_string()]),
        ] {
            let case = format!("{label} ({bad})");
            let (status, body) = ec2_query::call(
                &router,
                REGION,
                "EnableApplicationStatusCheckSuppression",
                &instance_fields(&batch),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status}: {body}"
            );
            let code = ec2_query::error_code(&body);
            assert_eq!(
                code, "InvalidInstanceID.Malformed",
                "case {case:?}: expected InvalidInstanceID.Malformed, got {code:?} in {body}"
            );
            let message = ec2_query::error_message(&body);
            assert_eq!(
                message,
                format!("The instance ID '{bad}' is malformed"),
                "case {case:?}: the refusal must name the id it could not resolve, got \
                 {message:?}"
            );
            let status_of_known = described_status(&case, &router, &known).await;
            assert_ne!(
                status_of_known.as_deref(),
                Some("suppressed"),
                "case {case:?}: a refused batch must suppress nothing, got {status_of_known:?}"
            );
        }
    }
}

/// Rule: a request the enable cannot act on at all is refused with EC2's own code — no
/// instance named, more than the documented ceiling, a duration that counts no seconds — and
/// a dry run answers the probe.
#[tokio::test]
async fn the_enable_refuses_a_request_it_cannot_act_on() {
    let (router, _engine) = ec2_query::emulator();
    let known = launch_one(&router).await;

    let too_many: Vec<String> = (0..101)
        .map(|nth| format!("i-{:017x}", nth + 1))
        .collect();
    let rows: Vec<Refusal> = vec![
        (
            "no instance at all",
            Vec::new(),
            400,
            "MissingParameter",
        ),
        (
            "more instances than the documented ceiling",
            instance_fields(&too_many),
            400,
            "InvalidParameterValue",
        ),
        (
            "a duration that counts no seconds",
            {
                let mut params = instance_fields(std::slice::from_ref(&known));
                params.push(p("DurationSeconds", "0"));
                params
            },
            400,
            "InvalidParameterValue",
        ),
        (
            "a dry run naming the instance",
            {
                let mut params = instance_fields(std::slice::from_ref(&known));
                params.push(p("DryRun", "true"));
                params
            },
            412,
            "DryRunOperation",
        ),
    ];

    for (case, params, status_wanted, code_wanted) in rows {
        let (status, body) = ec2_query::call(
            &router,
            REGION,
            "EnableApplicationStatusCheckSuppression",
            &params,
        )
        .await;
        assert_eq!(
            status, status_wanted,
            "case {case:?}: expected HTTP {status_wanted}, got {status}: {body}"
        );
        let code = ec2_query::error_code(&body);
        assert_eq!(
            code, code_wanted,
            "case {case:?}: expected {code_wanted}, got {code:?} in {body}"
        );
        let reported = described_status(case, &router, &known).await;
        assert_ne!(
            reported.as_deref(),
            Some("suppressed"),
            "case {case:?}: a refused request must suppress nothing, got {reported:?}"
        );
    }
}

// --- the disassociation and the associations describe -----------------------

/// Every association the describe reports, as `(check id, association type, key, value)`.
async fn described_associations(
    case: &str,
    router: &MultiServiceRouter,
    params: &[(String, String)],
) -> Vec<(String, String, Option<String>, Option<String>)> {
    let (status, body) = ec2_query::call(
        router,
        REGION,
        "DescribeApplicationStatusCheckAssociations",
        params,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the associations describe must succeed, got {status}: {body}"
    );
    let parsed = document(case, &body);
    set_items(parsed.root_element(), "associationSet")
        .into_iter()
        .map(|item| {
            (
                child_text(item, "applicationStatusCheckId").unwrap_or_default(),
                child_text(item, "associationType").unwrap_or_default(),
                child_text(item, "key"),
                child_text(item, "value"),
            )
        })
        .collect()
}

/// Rule: a disassociation removes the association itself, so every reader stops reporting it
/// — and an item that was not associated fails on its own row instead of refusing the rest.
#[tokio::test]
async fn a_disassociation_removes_what_the_readers_report() {
    let case = "disassociation round trip";
    let (router, _engine) = ec2_query::emulator();
    let check_id = create_check(&router).await;
    let instance_id = launch_one(&router).await;
    let other_instance = launch_one(&router).await;

    let mut associate = vec![p("ApplicationStatusCheckId", check_id.clone())];
    associate.extend(instance_fields(&[instance_id.clone(), other_instance.clone()]));
    let (status, body) =
        ec2_query::call(&router, REGION, "AssociateApplicationStatusCheck", &associate).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the instance association must succeed, got {status}: {body}"
    );
    let mut associate = vec![p("ApplicationStatusCheckId", check_id.clone())];
    associate.extend(tag_fields(&[("Env", "prod"), ("Tier", "web")]));
    let (status, body) =
        ec2_query::call(&router, REGION, "AssociateApplicationStatusCheck", &associate).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the tag association must succeed, got {status}: {body}"
    );

    // Both halves are reported, one row each.
    let reported = described_associations(
        case,
        &router,
        &[p("ApplicationStatusCheckId.1", check_id.clone())],
    )
    .await;
    let seen: BTreeSet<(String, String, Option<String>, Option<String>)> =
        reported.iter().cloned().collect();
    let expected: BTreeSet<(String, String, Option<String>, Option<String>)> = [
        (
            check_id.clone(),
            ASSOCIATION_TYPE_TAG.to_string(),
            Some("Env".to_string()),
            Some("prod".to_string()),
        ),
        (
            check_id.clone(),
            ASSOCIATION_TYPE_TAG.to_string(),
            Some("Tier".to_string()),
            Some("web".to_string()),
        ),
        (
            check_id.clone(),
            ASSOCIATION_TYPE_INSTANCE.to_string(),
            None,
            Some(instance_id.clone()),
        ),
        (
            check_id.clone(),
            ASSOCIATION_TYPE_INSTANCE.to_string(),
            None,
            Some(other_instance.clone()),
        ),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        seen, expected,
        "case {case:?}: the describe must report one row per association"
    );

    // The one filter the operation documents selects by association type.
    for (association_type, wanted) in [(ASSOCIATION_TYPE_TAG, 2), (ASSOCIATION_TYPE_INSTANCE, 2)] {
        let rows = described_associations(
            case,
            &router,
            &ec2_query::filters(&[("association-type", &[association_type])]),
        )
        .await;
        assert_eq!(
            rows.len(),
            wanted,
            "case {case:?}: the {association_type} filter must select {wanted} rows, got \
             {rows:?}"
        );
        assert!(
            rows.iter().all(|row| row.1 == association_type),
            "case {case:?}: the filter must select only {association_type} rows, got {rows:?}"
        );
    }

    // One page at a time, which must cover every row once.
    let mut token: Option<String> = None;
    let mut paged: BTreeSet<(String, String, Option<String>, Option<String>)> = BTreeSet::new();
    for _ in 0..8 {
        let mut params = vec![p("MaxResults", "5")];
        if let Some(next) = token.clone() {
            params.push(p("NextToken", next));
        }
        let (status, body) = ec2_query::call(
            &router,
            REGION,
            "DescribeApplicationStatusCheckAssociations",
            &params,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a paged describe must succeed, got {status}: {body}"
        );
        let parsed = document(case, &body);
        for item in set_items(parsed.root_element(), "associationSet") {
            paged.insert((
                child_text(item, "applicationStatusCheckId").unwrap_or_default(),
                child_text(item, "associationType").unwrap_or_default(),
                child_text(item, "key"),
                child_text(item, "value"),
            ));
        }
        token = ec2_query::member(&body, "nextToken");
        if token.is_none() {
            break;
        }
    }
    assert_eq!(
        paged, expected,
        "case {case:?}: paging must cover every association once"
    );

    // Disassociating one instance and one tag pair.
    let mut params = vec![p("ApplicationStatusCheckId", check_id.clone())];
    params.extend(instance_fields(std::slice::from_ref(&instance_id)));
    let (status, body) = ec2_query::call(
        &router,
        REGION,
        "DisassociateApplicationStatusCheck",
        &params,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the instance disassociation must succeed, got {status}: {body}"
    );
    let mut params = vec![p("ApplicationStatusCheckId", check_id.clone())];
    params.extend(tag_fields(&[("Env", "prod")]));
    let (status, body) = ec2_query::call(
        &router,
        REGION,
        "DisassociateApplicationStatusCheck",
        &params,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the tag disassociation must succeed, got {status}: {body}"
    );

    let rows = described_associations(
        case,
        &router,
        &[p("ApplicationStatusCheckId.1", check_id.clone())],
    )
    .await;
    let seen: BTreeSet<(String, String, Option<String>, Option<String>)> =
        rows.iter().cloned().collect();
    let left: BTreeSet<(String, String, Option<String>, Option<String>)> = [
        (
            check_id.clone(),
            ASSOCIATION_TYPE_TAG.to_string(),
            Some("Tier".to_string()),
            Some("web".to_string()),
        ),
        (
            check_id.clone(),
            ASSOCIATION_TYPE_INSTANCE.to_string(),
            None,
            Some(other_instance.clone()),
        ),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        seen, left,
        "case {case:?}: only the associations nothing removed are left"
    );

    // The instance that was disassociated holds no result of this check any more, which is
    // what the status reader reports.
    let (status, body) = ec2_query::call(
        &router,
        REGION,
        "DescribeApplicationStatus",
        &[p("InstanceId.1", instance_id.clone())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeApplicationStatus must succeed, got {status}: {body}"
    );
    assert!(
        !body.contains(check_id.as_str()),
        "case {case:?}: the disassociated instance must report no result of {check_id}: {body}"
    );

    // Repeating the disassociation finds nothing to remove: the item fails on its own row.
    let mut params = vec![p("ApplicationStatusCheckId", check_id.clone())];
    params.extend(instance_fields(std::slice::from_ref(&instance_id)));
    let (status, body) = ec2_query::call(
        &router,
        REGION,
        "DisassociateApplicationStatusCheck",
        &params,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a repeated disassociation must answer, got {status}: {body}"
    );
    let parsed = document(case, &body);
    let unsuccessful = set_items(parsed.root_element(), "unsuccessfulResultSet");
    assert_eq!(
        unsuccessful.len(),
        1,
        "case {case:?}: the item nothing was associated for must be unsuccessful: {body}"
    );
    assert!(
        child_text(unsuccessful[0], "reason").is_some(),
        "case {case:?}: an unsuccessful row carries the reason: {body}"
    );
}

/// Rule: the disassociation is held to the rules it publishes — exactly one of the two target
/// members, a check of the account, no blank tag key — and a dry run answers the probe
/// without removing anything.
#[tokio::test]
async fn the_disassociation_refuses_a_request_it_cannot_act_on() {
    let (router, _engine) = ec2_query::emulator();
    let check_id = create_check(&router).await;
    let instance_id = launch_one(&router).await;

    let mut associate = vec![p("ApplicationStatusCheckId", check_id.clone())];
    associate.extend(instance_fields(std::slice::from_ref(&instance_id)));
    let (status, body) =
        ec2_query::call(&router, REGION, "AssociateApplicationStatusCheck", &associate).await;
    assert_eq!(
        status, 200,
        "the association must succeed, got {status}: {body}"
    );

    let rows: Vec<CheckRefusal> = vec![
        (
            "both target members",
            check_id.clone(),
            {
                let mut params = instance_fields(std::slice::from_ref(&instance_id));
                params.extend(tag_fields(&[("Env", "prod")]));
                params
            },
            400,
            "InvalidParameterCombination",
        ),
        (
            "neither target member",
            check_id.clone(),
            Vec::new(),
            400,
            "MissingParameter",
        ),
        (
            "a blank tag key",
            check_id.clone(),
            tag_fields(&[("", "prod")]),
            400,
            "InvalidParameterValue",
        ),
        (
            "more tag pairs than a check holds associations for",
            check_id.clone(),
            {
                // One past the documented ceiling of tag associations a check holds, so no
                // request this long could name associations it has — refused before any work
                // proportional to it is done.
                let pairs: Vec<(String, String)> = (0..=MAX_APPLICATION_STATUS_CHECK_TAG_ASSOCIATIONS)
                    .map(|nth| (format!("Key{nth}"), format!("Value{nth}")))
                    .collect();
                let borrowed: Vec<(&str, &str)> = pairs
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str()))
                    .collect();
                tag_fields(&borrowed)
            },
            400,
            "InvalidParameterValue",
        ),
        (
            "a check id the account holds no check for",
            "asc-0123456789abcdef0".to_string(),
            instance_fields(std::slice::from_ref(&instance_id)),
            400,
            "InvalidApplicationStatusCheckId.NotFound",
        ),
        (
            "a value that is no check id at all",
            "check-7".to_string(),
            instance_fields(std::slice::from_ref(&instance_id)),
            400,
            "InvalidApplicationStatusCheckId.Malformed",
        ),
        (
            "a dry run that would have succeeded",
            check_id.clone(),
            {
                let mut params = instance_fields(std::slice::from_ref(&instance_id));
                params.push(p("DryRun", "true"));
                params
            },
            412,
            "DryRunOperation",
        ),
    ];

    for (case, check, fields, status_wanted, code_wanted) in rows {
        let mut params = vec![p("ApplicationStatusCheckId", check)];
        params.extend(fields);
        let (status, body) = ec2_query::call(
            &router,
            REGION,
            "DisassociateApplicationStatusCheck",
            &params,
        )
        .await;
        assert_eq!(
            status, status_wanted,
            "case {case:?}: expected HTTP {status_wanted}, got {status}: {body}"
        );
        let code = ec2_query::error_code(&body);
        assert_eq!(
            code, code_wanted,
            "case {case:?}: expected {code_wanted}, got {code:?} in {body}"
        );
        // Nothing was removed: the association the setup made is still reported.
        let rows = described_associations(
            case,
            &router,
            &[p("ApplicationStatusCheckId.1", check_id.clone())],
        )
        .await;
        assert_eq!(
            rows.len(),
            1,
            "case {case:?}: the refused call must remove nothing, got {rows:?}"
        );
    }
}

/// Rule: both halves of the pair publish the SAME rule about their two target members, so
/// both answer the same code and the same sentence for a request that names both.
#[tokio::test]
async fn both_halves_of_the_pair_word_the_target_rule_one_way() {
    let (router, _engine) = ec2_query::emulator();
    let check_id = create_check(&router).await;
    let instance_id = launch_one(&router).await;

    let expected = "Only one of TargetTagAssociations or InstanceIds may be specified, not both";
    for action in [
        "AssociateApplicationStatusCheck",
        "DisassociateApplicationStatusCheck",
    ] {
        let mut params = vec![p("ApplicationStatusCheckId", check_id.clone())];
        params.extend(instance_fields(std::slice::from_ref(&instance_id)));
        params.extend(tag_fields(&[("Env", "prod")]));
        let (status, body) = ec2_query::call(&router, REGION, action, &params).await;
        assert_eq!(
            status, 400,
            "case {action:?}: expected HTTP 400, got {status}: {body}"
        );
        let code = ec2_query::error_code(&body);
        assert_eq!(
            code, "InvalidParameterCombination",
            "case {action:?}: expected InvalidParameterCombination, got {code:?} in {body}"
        );
        let message = ec2_query::error_message(&body);
        assert_eq!(
            message, expected,
            "case {action:?}: expected message {expected:?}, got {message:?}"
        );
    }
}
