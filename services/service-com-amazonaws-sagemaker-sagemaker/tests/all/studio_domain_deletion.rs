// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for SageMaker's Studio domain TEARDOWN: `DeleteDomain`, and
//! what `DescribeDomain` and `ListDomains` report while and after it runs.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, so
//! what these tests exercise is what a real SDK client gets. The harness (`Emulator`,
//! `error_code`, `message`, `str_field`) is the one `inference_deployment_lifecycle`
//! already defines for this target.
//!
//! The tests assert RULES over a TABLE of inputs rather than one recorded call: the
//! teardown is driven for every `RetentionType` the model declares and for none, across
//! Regions, both `AuthMode` members, several name shapes and from both the status a
//! domain comes up in and the one the clock moves it to; the refusals are driven for
//! every id a domain cannot be resolved under, for a repeat of the call, and for a domain
//! that still holds apps created under each owner member.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Map, Value};
use service_com_amazonaws_sagemaker_sagemaker::state::{
    ComAmazonawsSagemakerSagemakerState, DOMAIN_STATUS_DELETING, DOMAIN_STATUS_IN_SERVICE,
    DOMAIN_STATUS_PENDING,
};

use crate::hyperpod_cluster_audit_record::audit_capture;
use crate::inference_deployment_lifecycle::{error_code, message, str_field, Emulator, ACCOUNT};

/// An execution role inside `RoleArn`'s modelled pattern, for the `DefaultUserSettings`
/// every create must carry.
const EXECUTION_ROLE: &str = "arn:aws:iam::123456789012:role/service-role/StudioRole";

/// Every value `RetentionType` accepts, from the model
/// (`smithy_query(query="shape", shape="RetentionType", model_path=".../com-amazonaws-sagemaker-sagemaker.json")`),
/// which is what `DeleteDomain$RetentionPolicy.HomeEfsFileSystem` is typed by.
const RETENTION_TYPES: [&str; 2] = ["Retain", "Delete"];

/// How long a poll below gives the clock to settle a domain: far longer than the spans
/// the state gives a domain's provisioning and its teardown, so a loaded host still
/// settles well inside it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How often a poll looks again. Only the loop's condition decides when it stops.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The smallest `CreateDomain` request: the three required members and nothing else.
fn minimal(domain_name: &str, auth_mode: &str) -> Value {
    json!({
        "DomainName": domain_name,
        "AuthMode": auth_mode,
        "DefaultUserSettings": { "ExecutionRole": EXECUTION_ROLE }
    })
}

/// `base` with every member of `extra` added.
fn with_members(base: &Value, extra: &Value) -> Value {
    let mut merged = base
        .as_object()
        .expect("the base members form an object")
        .clone();
    for (member, value) in extra.as_object().expect("the extra members form an object") {
        merged.insert(member.clone(), value.clone());
    }
    Value::Object(merged)
}

/// The ARN `DomainArn`'s modelled pattern gives the domain `domain_id` in `region`.
fn domain_arn(region: &str, domain_id: &str) -> String {
    format!("arn:aws:sagemaker:{region}:{ACCOUNT}:domain/{domain_id}")
}

/// The `DomainId`s a `ListDomains` response lists, in response order.
fn listed_ids(body: &Value) -> Vec<String> {
    body.get("Domains")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected a Domains list in {body}"))
        .iter()
        .map(|details| str_field(details, "DomainId"))
        .collect()
}

/// Walk `ListDomains` in `region` to the end, answering every listed `DomainId`.
async fn all_listed_ids(emulator: &Emulator, region: &str) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let request = match &token {
            None => json!({}),
            Some(token) => json!({ "NextToken": token }),
        };
        let (status, page) = emulator.call_in(region, "ListDomains", &request).await;
        assert_eq!(
            status, 200,
            "case list-{region}: expected HTTP 200 from ListDomains, got {status} with {page}"
        );
        ids.extend(listed_ids(&page));
        match page.get("NextToken").and_then(Value::as_str) {
            Some(next) => token = Some(next.to_string()),
            None => break,
        }
    }
    ids
}

/// One row of the teardown table: the create to tear down, the Region it is signed for,
/// the `DeleteDomain` members beyond `DomainId`, and whether the row waits for the clock
/// to bring the domain up before asking for its deletion.
struct Teardown {
    case: &'static str,
    region: &'static str,
    create: Value,
    delete_extra: Value,
    wait_for_in_service: bool,
}

/// The instant the store says `domain_id`'s removal is due, or `None` when no teardown
/// has been asked for.
///
/// Read straight from the shared engine the handler writes (the seam
/// `hyperpod_cluster_audit_record` reads and seeds the same store through), so a case can
/// tell a teardown the clock had already completed from one it had not instead of
/// depending on how fast the host runs.
fn removal_due_at(emulator: &Emulator, domain_id: &str) -> Option<SystemTime> {
    let store = emulator
        .engine
        .state::<ComAmazonawsSagemakerSagemakerState>();
    let store = store.lock();
    store
        .domains
        .get(domain_id)
        .and_then(|domain| domain.removal_due_at)
}

/// Assert what a reader reports for a domain whose teardown has just been asked for: the
/// `Deleting` status while the clock still owes the removal, and the not-found refusal
/// once that instant has passed.
///
/// WHICH of the two is correct is read from the stored due instant rather than assumed, so
/// a host slow enough to pass that instant between two calls still exercises the rule
/// instead of failing on it.
async fn assert_tearing_down(emulator: &Emulator, region: &str, case: &str, domain_id: &str) {
    let (status, parsed) = emulator
        .call_in(region, "DescribeDomain", &json!({ "DomainId": domain_id }))
        .await;
    // The reader resolves a torn-down domain without dropping the record, so the due
    // instant the delete stored is still readable whichever answer came back.
    let due = removal_due_at(emulator, domain_id);
    assert!(
        due.is_some(),
        "case {case}: the served delete must leave the clock owing this domain's removal, \
         store says {due:?}"
    );
    if status == 200 {
        assert_eq!(
            str_field(&parsed, "Status"),
            DOMAIN_STATUS_DELETING,
            "case {case}: a domain still being torn down reports {DOMAIN_STATUS_DELETING}, \
             got {parsed}"
        );
        return;
    }
    assert_eq!(
        (status, error_code(&parsed).as_str()),
        (400, "ResourceNotFound"),
        "case {case}: the only other answer is the not-found refusal, got {status} with \
         {parsed}"
    );
    assert!(
        due.is_some_and(|due| SystemTime::now() >= due),
        "case {case}: a domain reported gone must be one whose removal the clock already \
         owed, due {due:?}, got {parsed}"
    );
}

/// Poll `DescribeDomain` in `region` until it refuses `domain_id`, answering the refusal.
async fn poll_until_gone(emulator: &Emulator, region: &str, case: &str, domain_id: &str) -> Value {
    let describe = json!({ "DomainId": domain_id });
    // Wait for the CONDITION, not for a fixed time.
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, parsed) = emulator.call_in(region, "DescribeDomain", &describe).await;
        if status == 400 {
            return parsed;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: the domain must be gone once the clock completes the teardown, \
             last read {status} {parsed}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: `DeleteDomain` tears down the domain its `DomainId` names — the domain reports
/// the non-terminal `Deleting` status from the call, and once the clock completes the
/// removal no reader resolves that id any more — whatever retention the request states
/// and whatever status the domain was in when the call arrived.
#[tokio::test]
async fn delete_domain_tears_down_the_domain_it_names_on_the_clock() {
    // Installed BEFORE the first call, because the audit records asserted at the end of
    // this test are only captured from the moment this binary's one capturing subscriber
    // exists.
    let audit = audit_capture();
    let emulator = Emulator::new();

    // Rows differ along what the rule generalises over: the Region signed for, the
    // `AuthMode` the domain was created with, the name shape (one character, hyphenated,
    // the 63-character maximum `DomainName` admits), which optional members the create
    // carried, every `RetentionType` the model declares plus a request that states no
    // retention at all, and the status the domain is in when the delete arrives.
    let longest_name = format!("a{}", "b".repeat(62));
    let mut rows: Vec<Teardown> = vec![
        Teardown {
            case: "no-retention-policy-while-pending",
            region: "us-east-1",
            create: minimal("studio", "IAM"),
            delete_extra: json!({}),
            wait_for_in_service: false,
        },
        Teardown {
            case: "single-character-name-after-the-clock-brought-it-up",
            region: "eu-west-1",
            create: minimal("a", "SSO"),
            delete_extra: json!({}),
            wait_for_in_service: true,
        },
        Teardown {
            case: "optional-members-and-tags",
            region: "ap-northeast-1",
            create: with_members(
                &minimal("studio-with-options", "IAM"),
                &json!({
                    "AppNetworkAccessType": "PublicInternetOnly",
                    "HomeEfsFileSystemCreation": "Disabled",
                    "TagPropagation": "ENABLED",
                    "KmsKeyId": "alias/studio",
                    "Tags": [{ "Key": "team", "Value": "research" }]
                }),
            ),
            delete_extra: json!({}),
            wait_for_in_service: false,
        },
        Teardown {
            case: "longest-name",
            region: "us-west-2",
            create: minimal(&longest_name, "IAM"),
            delete_extra: json!({}),
            wait_for_in_service: false,
        },
    ];
    // One row per `RetentionType` the model declares, so the teardown is driven for every
    // member of that catalog rather than for the one a recording happened to carry.
    for retention in RETENTION_TYPES {
        rows.push(Teardown {
            case: match retention {
                "Retain" => "retention-retain",
                _ => "retention-delete",
            },
            region: "us-east-2",
            create: minimal(&format!("studio-{}", retention.to_lowercase()), "IAM"),
            delete_extra: json!({ "RetentionPolicy": { "HomeEfsFileSystem": retention } }),
            wait_for_in_service: false,
        });
    }

    // Every row is created and torn down first, and the removals are polled for
    // afterwards, so the one clock span the state owes each teardown is waited out once
    // rather than once per row.
    let mut torn_down: Vec<(&'static str, &'static str, String)> = Vec::new();
    for row in &rows {
        let created = emulator
            .call_in(row.region, "CreateDomain", &row.create)
            .await;
        assert_eq!(
            created.0, 200,
            "case {}: expected HTTP 200 from CreateDomain, got {} with {}",
            row.case, created.0, created.1
        );
        let domain_id = str_field(&created.1, "DomainId");
        let describe = json!({ "DomainId": domain_id });

        if row.wait_for_in_service {
            let deadline = Instant::now() + SETTLE_DEADLINE;
            loop {
                let described = emulator
                    .call_in(row.region, "DescribeDomain", &describe)
                    .await;
                if str_field(&described.1, "Status") == DOMAIN_STATUS_IN_SERVICE {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "case {}: the domain must reach {DOMAIN_STATUS_IN_SERVICE} before the \
                     delete, last read {}",
                    row.case,
                    described.1
                );
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        } else {
            // No wait: the row tears the domain down from whichever status the clock has
            // given it by now. Which of the two that is belongs to the create's own rule
            // (`studio_domain_registry`), so what matters here is only that the domain is
            // a live one — a loaded host must not make this row depend on how fast it
            // runs.
            let described = emulator
                .call_in(row.region, "DescribeDomain", &describe)
                .await;
            let status = str_field(&described.1, "Status");
            assert!(
                status == DOMAIN_STATUS_PENDING || status == DOMAIN_STATUS_IN_SERVICE,
                "case {}: a created domain is live before the delete, got {}",
                row.case,
                described.1
            );
        }

        let delete_request = with_members(&describe, &row.delete_extra);
        let (status, deleted) = emulator
            .call_in(row.region, "DeleteDomain", &delete_request)
            .await;
        assert_eq!(
            status, 200,
            "case {}: expected HTTP 200 from DeleteDomain, got {status} with {deleted}",
            row.case
        );
        assert!(
            deleted.as_object().is_none_or(Map::is_empty),
            "case {}: DeleteDomain answers no members, got {deleted}",
            row.case
        );

        // The teardown is the clock's, so the transient status is observable: the record
        // is still there and reports `Deleting` until the removal falls due.
        assert_tearing_down(&emulator, row.region, row.case, &domain_id).await;

        torn_down.push((row.case, row.region, domain_id));
    }

    // A teardown is a change to this service's state, so each one leaves the structured
    // audit record `state::record_state_change` emits — read back through the capture this
    // test binary shares, scoped to the ARNs these rows minted.
    let torn_down_arns: Vec<String> = torn_down
        .iter()
        .map(|(_, region, domain_id)| domain_arn(region, domain_id))
        .collect();
    for (index, (case, region, domain_id)) in torn_down.iter().enumerate() {
        let arn = domain_arn(region, domain_id);
        let records = audit.records_for("DeleteDomain", &torn_down_arns[index..=index]);
        assert_eq!(
            records.len(),
            1,
            "case {case}: the teardown leaves exactly one audit record for {arn}, got \
             {records:?}"
        );
        assert_eq!(
            records[0].field("resource"),
            Some(arn.as_str()),
            "case {case}: the audit record names the domain it changed, got {records:?}"
        );
        assert_eq!(
            records[0].field("access_key_id"),
            Some("AKIDEXAMPLE"),
            "case {case}: the audit record attributes the call to the presented access key \
             id, got {records:?}"
        );
    }

    for (case, region, domain_id) in &torn_down {
        let refusal = poll_until_gone(&emulator, region, case, domain_id).await;
        assert_eq!(
            error_code(&refusal),
            "ResourceNotFound",
            "case {case}: the reader refuses a torn-down domain, got {refusal}"
        );
        assert_eq!(
            message(&refusal),
            format!("Domain [{}] does not exist.", domain_arn(region, domain_id)),
            "case {case}: the refusal names the domain it cannot resolve, got {refusal}"
        );
        assert!(
            !all_listed_ids(&emulator, region).await.contains(domain_id),
            "case {case}: a torn-down domain is listed nowhere"
        );
        // A gone domain cannot be torn down a second time either: the id resolves to
        // nothing for the delete exactly as it does for the reader.
        let (status, parsed) = emulator
            .call_in(region, "DeleteDomain", &json!({ "DomainId": domain_id }))
            .await;
        assert_eq!(
            status, 400,
            "case {case}: deleting a gone domain is refused, got {status} with {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ResourceNotFound",
            "case {case}: deleting a gone domain is refused as not found, got {parsed}"
        );
    }
}

/// RULE: `DeleteDomain` refuses a `DomainId` this account holds no domain under with the
/// error the model declares for a resource that cannot be found, worded with the ARN the
/// requested id names in the Region the caller signed for; a domain the service is
/// already tearing down is refused as in use instead, and neither refusal changes what a
/// later caller sees.
#[tokio::test]
async fn delete_domain_refuses_what_it_cannot_tear_down() {
    let emulator = Emulator::new();

    // Rows: well-formed ids nothing is stored under, in the Regions whose ARNs the
    // refusal has to name. `DomainId` matches `^d-(-*[a-z0-9]){1,61}$`, so each is inside
    // the modelled shape and reaches the handler.
    for (case, region, domain_id) in [
        ("absent-id", "us-east-1", "d-000000000000"),
        ("another-absent-id", "us-east-1", "d-abc123def456"),
        ("absent-id-in-another-region", "eu-west-1", "d-0a1b2c3d4e5f"),
        ("shortest-absent-id", "us-west-2", "d-a"),
    ] {
        let (status, parsed) = emulator
            .call_in(region, "DeleteDomain", &json!({ "DomainId": domain_id }))
            .await;
        assert_eq!(
            status, 400,
            "case {case}: the delete must refuse, got {status} with {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ResourceNotFound",
            "case {case}: expected ResourceNotFound, got {parsed}"
        );
        assert_eq!(
            message(&parsed),
            format!("Domain [{}] does not exist.", domain_arn(region, domain_id)),
            "case {case}: the refusal names the ARN the id resolves to, got {parsed}"
        );
    }

    // A repeat of the call reaches a domain the first one left `Deleting`: the teardown is
    // in flight, which is the in-use refusal this operation models.
    for (case, region) in [
        ("repeat-delete", "us-east-1"),
        ("repeat-delete-in-another-region", "eu-central-1"),
    ] {
        let created = emulator
            .call_in(region, "CreateDomain", &minimal("repeat", "IAM"))
            .await;
        let domain_id = str_field(&created.1, "DomainId");
        let request = json!({ "DomainId": domain_id });
        let (status, parsed) = emulator.call_in(region, "DeleteDomain", &request).await;
        assert_eq!(
            status, 200,
            "case {case}: the first delete is served, got {status} with {parsed}"
        );

        // The due instant the first delete stored, read before the repeat runs: a repeat
        // that arrives after it is one the clock had already finished the teardown for,
        // and the only answer left then is the not-found refusal. Which of the two a case
        // takes is read from the store rather than from how fast the host runs.
        let due = removal_due_at(&emulator, &domain_id);
        assert!(
            due.is_some(),
            "case {case}: the served delete must leave the clock owing the removal, store \
             says {due:?}"
        );

        let (status, parsed) = emulator.call_in(region, "DeleteDomain", &request).await;
        assert_eq!(
            status, 400,
            "case {case}: the repeat must refuse, got {status} with {parsed}"
        );
        if error_code(&parsed) == "ResourceInUse" {
            assert_eq!(
                message(&parsed),
                format!(
                    "Domain [{}] cannot be deleted while it is in {DOMAIN_STATUS_DELETING} status.",
                    domain_arn(region, &domain_id)
                ),
                "case {case}: the refusal names the status in the way, got {parsed}"
            );
            // The refused repeat changed nothing: the domain is still the one the first
            // call left on its way out.
            assert_tearing_down(&emulator, region, case, &domain_id).await;
        } else {
            assert_eq!(
                error_code(&parsed),
                "ResourceNotFound",
                "case {case}: the only other refusal is not-found, got {parsed}"
            );
            assert_eq!(
                message(&parsed),
                format!("Domain [{}] does not exist.", domain_arn(region, &domain_id)),
                "case {case}: the not-found refusal names the ARN the id resolves to, got \
                 {parsed}"
            );
            assert!(
                due.is_some_and(|due| SystemTime::now() >= due),
                "case {case}: a repeat answered not-found must arrive after the clock \
                 completed the teardown, due {due:?}, got {parsed}"
            );
        }
    }
}

/// RULE: a domain that still holds Studio apps is in use, so its teardown is refused with
/// the error the model declares for that and the domain stays as it was; once the apps are
/// deleted the same call is served.
#[tokio::test]
async fn delete_domain_refuses_a_domain_that_still_holds_apps() {
    let emulator = Emulator::new();

    // Rows differ along the owner member an app hangs off (`CreateApp` takes exactly one
    // of the two) and along how many apps the domain holds, which is what the rule
    // generalises over.
    for (case, apps) in [
        (
            "one-user-profile-app",
            vec![json!({ "UserProfileName": "studio-user", "AppType": "JupyterServer", "AppName": "default" })],
        ),
        (
            "one-space-app",
            vec![json!({ "SpaceName": "shared", "AppType": "KernelGateway", "AppName": "kernel" })],
        ),
        (
            "apps-under-both-owner-members",
            vec![
                json!({ "UserProfileName": "studio-user", "AppType": "JupyterServer", "AppName": "default" }),
                json!({ "SpaceName": "shared", "AppType": "CodeEditor", "AppName": "editor" }),
            ],
        ),
    ] {
        let created = emulator.ok("CreateDomain", &minimal("hosting", "IAM")).await;
        let domain_id = str_field(&created, "DomainId");
        let request = json!({ "DomainId": domain_id });
        for app in &apps {
            let body = with_members(&request, app);
            let (status, parsed) = emulator.call("CreateApp", &body).await;
            assert_eq!(
                status, 200,
                "case {case}: expected HTTP 200 from CreateApp {body}, got {status} with {parsed}"
            );
        }

        let (status, parsed) = emulator.call("DeleteDomain", &request).await;
        assert_eq!(
            status, 400,
            "case {case}: a domain that still holds apps is not torn down, got {status} \
             with {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ResourceInUse",
            "case {case}: expected ResourceInUse, got {parsed}"
        );
        assert_eq!(
            message(&parsed),
            format!(
                "Domain [{}] has non-deleted Apps.",
                domain_arn("us-east-1", &domain_id)
            ),
            "case {case}: the refusal names the domain it cannot tear down, got {parsed}"
        );
        // The refusal left the domain alone: it is not on its way out, so a reader still
        // reports the status the clock gave it rather than `Deleting`.
        let described = emulator.ok("DescribeDomain", &request).await;
        assert_ne!(
            str_field(&described, "Status"),
            DOMAIN_STATUS_DELETING,
            "case {case}: a refused teardown starts no teardown, got {described}"
        );

        // Releasing the apps is what unblocks the call, so the same request is served
        // once the domain holds none.
        for (index, app) in apps.iter().enumerate() {
            let body = with_members(&request, app);
            let (status, parsed) = emulator.call("DeleteApp", &body).await;
            assert_eq!(
                status, 200,
                "case {case}: expected HTTP 200 from DeleteApp {body}, got {status} with {parsed}"
            );
            // While any app is left the teardown is still refused, so the refusal tracks
            // what the domain holds rather than what it held once.
            if index + 1 < apps.len() {
                let (status, parsed) = emulator.call("DeleteDomain", &request).await;
                assert_eq!(
                    status, 400,
                    "case {case}: the teardown is refused while {} app(s) are left, got \
                     {status} with {parsed}",
                    apps.len() - index - 1
                );
            }
        }

        let (status, parsed) = emulator.call("DeleteDomain", &request).await;
        assert_eq!(
            status, 200,
            "case {case}: the teardown is served once the apps are gone, got {status} \
             with {parsed}"
        );
        assert_tearing_down(&emulator, "us-east-1", case, &domain_id).await;
    }
}
