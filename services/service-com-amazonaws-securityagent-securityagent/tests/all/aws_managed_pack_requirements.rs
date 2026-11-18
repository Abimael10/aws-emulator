// SPDX-License-Identifier: CC0-1.0

//! The CONTENT of the AWS managed security requirement packs of the SecurityAgent
//! emulator, end to end over the wire: what `ListSecurityRequirements` and
//! `BatchGetSecurityRequirements` report for the requirements a fresh account's managed
//! packs already hold.
//!
//! Every call goes through the real `MultiServiceRouter` the emulator boots (see
//! `crate::support`), as a `restJson1` `POST /<OperationName>` with a JSON body signed
//! for the `securityagent` signing name, so a value counts only once it has come back out
//! of the shared store.
//!
//! RULE under test: a pack whose `managementType` is `AWS_MANAGED` is one AWS itself
//! ships, CONTENT INCLUDED — every read of it reports the security requirements that pack
//! ships, each a full `SecurityRequirementSummary` (`packId`, `name`, `description`,
//! `createdAt`, `updatedAt`, all `@required`) projected out of the one requirement store a
//! created requirement is read from, with both stamps the instant that pack's content was
//! published rather than a clock reading from this run. The table is the catalog itself
//! (`AWS_MANAGED_PACKS` crossed with the requirements each entry ships), so an entry added
//! to it is covered without these cases changing; the other cases' rows differ along the
//! reader (listing against batch read), the page size, the boot the answer came from, and
//! whether the requirement is one AWS ships or one a caller created.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use service_com_amazonaws_securityagent_securityagent::state::{
    aws_managed_pack_delete_refusal_message, AwsManagedPack, AWS_MANAGED_PACKS, MAX_PAGE_SIZE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::support::{call, emulator, member};

/// `call`, requiring `expected` as the HTTP status and naming the case when it is not.
async fn expect_status(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
    expected: u16,
    case: &str,
) -> Value {
    let answer = call(router, operation, body).await;
    assert_eq!(
        answer.status, expected,
        "case {case:?}: expected {operation} to answer {expected}, got {} ({}) with {}",
        answer.status, answer.error_code, answer.body
    );
    answer.body
}

/// Every summary `ListSecurityRequirements` reports for `pack_id`, paged to the end with
/// `page_size` (the service's own default when `None`).
async fn list_all(
    router: &MultiServiceRouter,
    pack_id: &str,
    page_size: Option<i32>,
    case: &str,
) -> Vec<Value> {
    let mut summaries = Vec::new();
    let mut token: Option<String> = None;
    // One more round than there can be pages of one entry each, so a cursor that never
    // retires fails the case instead of hanging it.
    for _ in 0..(MAX_PAGE_SIZE as usize + 2) {
        let mut request = json!({"packId": pack_id});
        if let Some(size) = page_size {
            request["maxResults"] = json!(size);
        }
        if let Some(token) = &token {
            request["nextToken"] = json!(token);
        }
        let payload = expect_status(router, "ListSecurityRequirements", &request, 200, case).await;
        let page = payload
            .get("securityRequirementSummaries")
            .and_then(Value::as_array)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: ListSecurityRequirements answered no \
                     securityRequirementSummaries list: {payload}"
                )
            })
            .clone();
        summaries.extend(page);
        match member(&payload, "nextToken") {
            Some(next) => token = Some(next),
            None => return summaries,
        }
    }
    panic!("case {case:?}: ListSecurityRequirements never stopped handing out cursors");
}

/// `BatchGetSecurityRequirements` for `names` inside `pack_id`, as the records it reports
/// keyed by name.
async fn batch_get(
    router: &MultiServiceRouter,
    pack_id: &str,
    names: &[&str],
    case: &str,
) -> BTreeMap<String, Value> {
    let payload = expect_status(
        router,
        "BatchGetSecurityRequirements",
        &json!({"packId": pack_id, "securityRequirementNames": names}),
        200,
        case,
    )
    .await;
    assert_eq!(
        payload.get("errors").and_then(Value::as_array),
        Some(&Vec::new()),
        "case {case:?}: expected no per-entry errors reading {names:?}, got {payload}"
    );
    payload
        .get("securityRequirements")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the batch read answered no securityRequirements: {payload}")
        })
        .iter()
        .map(|record| {
            (
                member(record, "name").unwrap_or_else(|| {
                    panic!("case {case:?}: a read record carries no name: {record}")
                }),
                record.clone(),
            )
        })
        .collect()
}

/// A modelled `date-time` member as epoch seconds.
///
/// `SecurityRequirementSummary$createdAt`/`$updatedAt` carry `timestampFormat: date-time`
/// (`smithy_query(query="shape", shape="SecurityRequirementSummary")`), which restJson1
/// renders as an ISO-8601 string; a number is read as epoch seconds too, so a case asserts
/// the VALUE rather than one rendering of it.
fn date_time(payload: &Value, name: &str, case: &str) -> f64 {
    let value = payload
        .get(name)
        .unwrap_or_else(|| panic!("case {case:?}: no {name} was reported: {payload}"));
    match value {
        Value::String(text) => aws_smithy_types::DateTime::from_str(
            text,
            aws_smithy_types::date_time::Format::DateTime,
        )
        .unwrap_or_else(|e| {
            panic!("case {case:?}: {name} {text:?} is not an RFC 3339 date-time: {e}")
        })
        .as_secs_f64(),
        Value::Number(number) => number.as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: {name} {number} is not a finite epoch timestamp")
        }),
        other => panic!("case {case:?}: {name} is not a modelled timestamp: {other}"),
    }
}

/// The instant the content of `pack` was published, as the epoch seconds both stamps of
/// each of its shipped requirements must carry.
fn content_published_at(pack: &AwsManagedPack) -> f64 {
    pack.content_published_at_epoch_seconds as f64
}

/// `CreateSecurityRequirementPack` over the wire, returning the minted `packId`. The model
/// gives the create `"http": {"code": 201}`.
async fn create_pack(router: &MultiServiceRouter, name: &str, case: &str) -> String {
    let created = expect_status(
        router,
        "CreateSecurityRequirementPack",
        &json!({"name": name}),
        201,
        case,
    )
    .await;
    member(&created, "packId")
        .unwrap_or_else(|| panic!("case {case:?}: no packId was minted: {created}"))
}

/// RULE: every pack AWS manages reports the requirements it ships, in name order, each a
/// full `SecurityRequirementSummary` of the content that pack ships — and the batch read
/// of the same name reports the same record with the domain, evaluation and remediation
/// beside it.
///
/// The table is the catalog itself: every entry of `AWS_MANAGED_PACKS`, including the
/// three packs the issue did not cite, crossed with every requirement that entry ships and
/// with the two readers that project a stored requirement.
#[tokio::test]
async fn every_managed_pack_reports_the_requirements_it_ships() {
    let router = emulator();

    for pack in AWS_MANAGED_PACKS {
        let case = pack.pack_id;
        assert!(
            !pack.requirements.is_empty(),
            "case {case:?}: a pack AWS manages ships content, so the catalog entry must \
             name at least one requirement"
        );

        let summaries = list_all(&router, pack.pack_id, None, case).await;
        let listed: Vec<String> = summaries
            .iter()
            .map(|summary| {
                member(summary, "name").unwrap_or_else(|| {
                    panic!("case {case:?}: a summary carries no name: {summary}")
                })
            })
            .collect();
        let mut shipped: Vec<String> = pack
            .requirements
            .iter()
            .map(|entry| entry.name.to_string())
            .collect();
        shipped.sort();
        assert_eq!(
            listed, shipped,
            "case {case:?}: expected the listing to report the shipped requirements in \
             name order"
        );

        let names: Vec<&str> = pack.requirements.iter().map(|entry| entry.name).collect();
        let read = batch_get(&router, pack.pack_id, &names, case).await;

        for entry in pack.requirements {
            let case = format!("{} / {}", pack.pack_id, entry.name);
            let summary = summaries
                .iter()
                .find(|summary| member(summary, "name").as_deref() == Some(entry.name))
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the listing does not report the shipped requirement")
                });
            let record = read.get(entry.name).unwrap_or_else(|| {
                panic!("case {case:?}: the batch read does not report the shipped requirement")
            });

            for (reader, payload) in [("the listing", summary), ("the batch read", record)] {
                assert_eq!(
                    member(payload, "packId").as_deref(),
                    Some(pack.pack_id),
                    "case {case:?} through {reader}: expected the owning packId, got {payload}"
                );
                assert_eq!(
                    member(payload, "description").as_deref(),
                    Some(entry.description),
                    "case {case:?} through {reader}: expected the shipped description {:?}, \
                     got {:?}",
                    entry.description,
                    member(payload, "description")
                );
            }

            // The members only the batch read carries: a shipped requirement is a full
            // record, not a name with a description.
            assert_eq!(
                member(record, "domain").as_deref(),
                Some(entry.domain),
                "case {case:?}: expected the shipped domain {:?}, got {record}",
                entry.domain
            );
            assert_eq!(
                member(record, "evaluation").as_deref(),
                Some(entry.evaluation),
                "case {case:?}: expected the shipped evaluation {:?}, got {record}",
                entry.evaluation
            );
            assert_eq!(
                member(record, "remediation").as_deref(),
                entry.remediation,
                "case {case:?}: expected the shipped remediation {:?}, got {record}",
                entry.remediation
            );
        }
    }
}

/// RULE: both required stamps of a shipped requirement are the instant that pack's
/// content was published — identical across the requirements of one pack, across the two
/// readers and across boots, because when AWS published the content is not when this
/// account's state was seeded; a requirement a CALLER creates still carries the clock.
///
/// The rows are every catalog entry's shipped requirements crossed with three
/// independently booted emulators (each boot seeds a fresh account's state, which is what
/// a restart does), plus a requirement created over the wire.
#[tokio::test]
async fn shipped_requirement_stamps_are_the_packs_content_instant_on_every_boot() {
    for boot in 0..3 {
        let router = emulator();
        for pack in AWS_MANAGED_PACKS {
            let wanted = content_published_at(&pack);
            let case = format!("boot {boot} pack {}", pack.pack_id);
            let summaries = list_all(&router, pack.pack_id, None, &case).await;
            let names: Vec<&str> = pack.requirements.iter().map(|entry| entry.name).collect();
            let read = batch_get(&router, pack.pack_id, &names, &case).await;

            for entry in pack.requirements {
                let case = format!("{case} / {}", entry.name);
                let summary = summaries
                    .iter()
                    .find(|summary| member(summary, "name").as_deref() == Some(entry.name))
                    .unwrap_or_else(|| panic!("case {case:?}: the listing lost the requirement"));
                let record = read
                    .get(entry.name)
                    .unwrap_or_else(|| panic!("case {case:?}: the batch read lost it"));

                for (reader, payload) in [("the listing", summary), ("the batch read", record)] {
                    for name in ["createdAt", "updatedAt"] {
                        let got = date_time(payload, name, &case);
                        assert!(
                            (got - wanted).abs() < f64::EPSILON,
                            "case {case:?} through {reader}: expected {name} to be the \
                             pack's content publication instant {wanted}, got {got}"
                        );
                    }
                }
            }
        }

        // A requirement the CALLER creates, in the same account at the same moment, is
        // stamped by the clock instead — so the instants above are the content's and not
        // a value every record of this store carries.
        let case = format!("boot {boot} a created requirement");
        let pack_id = create_pack(&router, "customer-pack", &case).await;
        let created = expect_status(
            &router,
            "BatchCreateSecurityRequirements",
            &json!({
                "packId": pack_id,
                "securityRequirements": [{
                    "name": "the-callers-own",
                    "description": "the caller's own text",
                    "domain": "DataProtection",
                    "evaluation": "check it"
                }]
            }),
            201,
            &case,
        )
        .await;
        let record = created
            .get("securityRequirements")
            .and_then(Value::as_array)
            .and_then(|records| records.first())
            .unwrap_or_else(|| panic!("case {case:?}: the create reported no record: {created}"))
            .clone();
        let newest = AWS_MANAGED_PACKS
            .iter()
            .map(|pack| pack.content_published_at_epoch_seconds)
            .max()
            .expect("the catalog is not empty") as f64;
        let got = date_time(&record, "createdAt", &case);
        assert!(
            got > newest,
            "case {case:?}: expected a requirement created now to be stamped after the \
             newest shipped content instant {newest}, got {got}"
        );
    }
}

/// RULE: a shipped requirement pages like any other — every page size reaches every
/// requirement of the pack exactly once, in name order, and only the last page carries no
/// cursor.
///
/// The rows are every catalog entry crossed with the page sizes 1, 3 and one above the
/// count the pack ships, so a pack's content is reached a page at a time and in one go.
#[tokio::test]
async fn shipped_requirements_page_like_any_others() {
    let router = emulator();

    for pack in AWS_MANAGED_PACKS {
        let whole: Vec<String> = list_all(&router, pack.pack_id, None, pack.pack_id)
            .await
            .iter()
            .map(|summary| member(summary, "name").expect("a summary carries a name"))
            .collect();

        for size in [1, 3, pack.requirements.len() as i32 + 1] {
            let case = format!("{} at {size} per page", pack.pack_id);
            let paged: Vec<String> = list_all(&router, pack.pack_id, Some(size), &case)
                .await
                .iter()
                .map(|summary| member(summary, "name").expect("a summary carries a name"))
                .collect();
            assert_eq!(
                paged, whole,
                "case {case:?}: paging must reach the same requirements in the same order \
                 as one unpaged listing"
            );
        }
    }
}

/// RULE: shipped content is the service's, not a caller's — a delete addressed to a pack
/// AWS manages is refused for the pack it names, whatever requirement it asks for, and the
/// pack's content is unchanged afterwards.
///
/// The rows are every catalog entry crossed with a requirement the pack really ships and a
/// name it does not hold, so the refusal is the pack's and not the name's.
#[tokio::test]
async fn shipped_content_is_not_a_callers_to_delete() {
    let router = emulator();

    for pack in AWS_MANAGED_PACKS {
        let before = list_all(&router, pack.pack_id, None, pack.pack_id).await;
        let shipped = pack.requirements[0].name;

        for (names, case) in [
            (vec![shipped], format!("{} shipped content", pack.pack_id)),
            (
                vec!["no-such-requirement"],
                format!("{} a name it does not hold", pack.pack_id),
            ),
            (
                vec![shipped, "no-such-requirement"],
                format!("{} both at once", pack.pack_id),
            ),
        ] {
            let answer = call(
                &router,
                "BatchDeleteSecurityRequirements",
                &json!({"packId": pack.pack_id, "securityRequirementNames": names}),
            )
            .await;
            assert_eq!(
                (answer.status, answer.error_code.as_str()),
                (400, "ValidationException"),
                "case {case:?}: expected the delete to be refused 400 ValidationException, \
                 got {} ({}) with {}",
                answer.status,
                answer.error_code,
                answer.body
            );
            // The sentence AWS answers for this refusal, written out here rather than taken
            // from the service's own helper, so a re-wording of the helper cannot change
            // both sides of the comparison at once. Its only variable part is the `packId`,
            // which the case interpolates from the request it just made.
            let wanted = format!(
                "Only customer managed packs support deleting security requirements. \
                 Pack ID: {}",
                pack.pack_id
            );
            assert_eq!(
                member(&answer.body, "message"),
                Some(wanted.clone()),
                "case {case:?}: expected the refusal {wanted:?}, got {}",
                answer.body
            );
            // And the service states that one sentence from one place, so every operation
            // that meets the condition answers it.
            assert_eq!(
                aws_managed_pack_delete_refusal_message(pack.pack_id),
                wanted,
                "case {case:?}: the shared refusal sentence must be the one AWS answers"
            );
        }

        let after = list_all(&router, pack.pack_id, None, pack.pack_id).await;
        assert_eq!(
            after, before,
            "case {:?}: a refused delete must leave the shipped content as it was",
            pack.pack_id
        );
    }
}

/// RULE: a requirement's name identifies it WITHIN its pack — a caller may create a
/// requirement of a name a pack AWS manages also ships, and the two records are read
/// independently of each other.
#[tokio::test]
async fn a_shipped_name_does_not_collide_with_a_callers_own() {
    let router = emulator();
    let case = "a name shared with shipped content";
    let managed = AWS_MANAGED_PACKS[0];
    let shared = managed.requirements[0].name;

    let pack_id = create_pack(&router, "customer-pack", case).await;
    expect_status(
        &router,
        "BatchCreateSecurityRequirements",
        &json!({
            "packId": pack_id,
            "securityRequirements": [{
                "name": shared,
                "description": "the caller's own text",
                "domain": "TheCallersOwnDomain",
                "evaluation": "the caller's own evaluation"
            }]
        }),
        201,
        case,
    )
    .await;

    let callers = batch_get(&router, &pack_id, &[shared], case).await;
    let shipped = batch_get(&router, managed.pack_id, &[shared], case).await;
    assert_eq!(
        member(&callers[shared], "description").as_deref(),
        Some("the caller's own text"),
        "case {case:?}: the caller's own record must report the caller's text: {:?}",
        callers[shared]
    );
    assert_eq!(
        member(&shipped[shared], "description").as_deref(),
        Some(managed.requirements[0].description),
        "case {case:?}: the shipped record must still report the shipped text: {:?}",
        shipped[shared]
    );
}

/// The requirement NAMES each AWS managed pack is published with, written out here rather
/// than read from the catalog table, so that a table which loses or gains an entry fails a
/// case instead of moving both sides of the comparison at once.
///
/// Source: the recorded content of each pack, as the
/// `securityagent-catalog-list-security-requirements` fixture reports it for `srp-am-base`
/// (step `task#1`), `srp-am-nist-csf` (`task#2`), `srp-am-pci-dss` (`task#3`) and
/// `srp-am-well-architected` (`task#4`) — the model enumerates neither which packs AWS
/// ships nor what they hold. Every entry of every pack is asserted, so the names the issue
/// never cited are covered as closely as the ones it did.
const PUBLISHED_REQUIREMENT_NAMES: [(&str, &[&str]); 4] = [
    (
        "srp-am-base",
        &[
            "audit-logging-best-practices",
            "authentication-best-practices",
            "authorization-best-practices",
            "information-protection-best-practices",
            "log-protection-best-practices",
            "privileged-access-best-practices",
            "secret-protection-best-practices",
            "secure-by-default-best-practices",
            "tenant-isolation-best-practices",
            "trusted-cryptography-best-practices",
        ],
    ),
    (
        "srp-am-nist-csf",
        &[
            "adverse-event-analysis-and-threat-intelligence-integration",
            "ai-component-risk-assessment-and-security-integration",
            "compliance-driven-workload-security-design",
            "comprehensive-logging-and-continuous-monitoring-coverage",
            "continuous-workload-cybersecurity-risk-management",
            "forensic-readiness-and-evidence-integrity-for-workloads",
            "holistic-cybersecurity-coverage-across-all-risk-functions",
            "incident-response-recovery-communication-planning",
            "least-privilege-and-separation-of-duties-for-workload-access",
            "platform-configuration-software-lifecycle-management",
            "privacy-cybersecurity-coordinated-risk-management",
            "risk-informed-security-control-selection-and-prioritization",
            "secure-software-development-lifecycle-integration",
            "security-control-mapping-to-recognized-frameworks",
            "security-design-consistency-and-repeatable-pattern-application",
            "sensitive-data-inventory-classification-and-flow-documentation",
            "software-supply-chain-integrity-and-acquisition-verification",
            "structured-incident-triage-escalation-and-recovery",
            "structured-post-incident-recovery-and-return-to-operations",
            "third-party-dependency-risk-assessment-and-governance",
            "user-third-party-activity-monitoring-for-anomaly-detection",
            "vulnerability-disclosure-and-change-risk-management",
            "workload-asset-inventory-and-lifecycle-management",
            "workload-backup-integrity-and-recovery-verification",
            "workload-continuous-monitoring-and-adverse-event-detection",
            "workload-cybersecurity-risk-assessment",
            "workload-cybersecurity-risk-prioritization-action-planning",
            "workload-data-lifecycle-cia-protection",
            "workload-identity-authentication-and-access-control",
            "workload-incident-containment-and-eradication-design",
            "workload-incident-recovery-architecture-and-communication",
            "workload-incident-response-readiness",
            "workload-infrastructure-resilience-architecture",
            "workload-network-protection-and-continuous-monitoring",
            "workload-resilience-and-availability-architecture",
            "workload-risk-visibility-and-communication-design",
            "workload-third-party-dependency-risk-management",
        ],
    ),
    (
        "srp-am-pci-dss",
        &[
            "account-data-classification-and-retention-controls",
            "anti-malware-protection-for-workload-components",
            "anti-phishing-controls-for-workload-communications",
            "application-and-system-account-security-management",
            "audit-log-protection-integrity-and-retention",
            "automated-audit-log-review-and-anomaly-detection",
            "cardholder-data-encryption-scope-isolation",
            "cde-network-segmentation-and-scope-minimization",
            "cde-network-segmentation-and-traffic-control",
            "cde-scope-documentation-and-data-flow-validation",
            "comprehensive-audit-logging-for-sensitive-data-access",
            "continuous-security-control-monitoring-and-failure-response",
            "cryptographic-agility-and-technology-lifecycle-risk",
            "cryptographic-key-management-for-stored-account-data",
            "documented-security-control-rationale-and-objectives",
            "incident-response-and-recovery-workflow",
            "intrusion-detection-file-integrity-and-page-tamper",
            "least-privilege-access-control-with-default-deny",
            "mfa-for-cardholder-data-environment-access",
            "multi-tenant-isolation-and-per-tenant-security",
            "network-architecture-and-data-flow-documentation",
            "network-config-security-and-internal-address-protection",
            "network-security-controls-for-traffic-segmentation",
            "pan-display-masking-and-storage-protection",
            "pci-dss-scope-documentation-and-change-impact-assessment",
            "penetration-testing-readiness-and-remediation",
            "poi-device-inventory-and-integrity-management",
            "programmatic-access-control-for-cardholder-data-stores",
            "protection-of-security-posture-artifacts",
            "public-facing-web-application-attack-protection",
            "regular-vulnerability-scanning-and-remediation",
            "sad-non-retention-after-authorization",
            "scheduled-security-activities-and-change-triggered-reviews",
            "secure-change-management-and-environment-separation",
            "secure-configuration-and-system-hardening",
            "secure-data-channel-scoping-and-unintended-data-handling",
            "secure-data-lifecycle-and-media-destruction",
            "secure-development-lifecycle-and-vulnerability-prevention",
            "secure-payment-software-lifecycle-and-configuration",
            "secure-wireless-network-configuration-for-cde",
            "session-idle-timeout-and-re-authentication",
            "stored-account-data-protection-and-retention",
            "strong-authentication-and-credential-management",
            "strong-cryptography-for-cardholder-data-in-transit",
            "strong-cryptography-for-pos-poi-terminal-connections",
            "third-party-service-provider-security-accountability",
            "time-synchronization",
            "unauthorized-network-access-path-detection",
            "unique-user-identification-and-account-lifecycle",
            "vulnerability-identification-and-patch-management",
        ],
    ),
    (
        "srp-am-well-architected",
        &[
            "authenticated-network-communication-architecture",
            "automated-certificate-management-architecture",
            "automated-credential-governance-architecture",
            "automated-data-classification-architecture",
            "automated-remediation-with-oversight-architecture",
            "automated-security-testing-architecture",
            "automated-threat-intelligence-integration",
            "automated-vulnerability-management-architecture",
            "automation-first-operational-access-architecture",
            "code-review-process-architecture",
            "compliance-framework-control-mapping",
            "comprehensive-security-logging-architecture",
            "controlled-cross-account-access-architecture",
            "data-at-rest-access-control-architecture",
            "data-classification-and-tagging-architecture",
            "data-lifecycle-management-architecture",
            "encryption-at-rest-enforcement-architecture",
            "encryption-in-transit-enforcement-architecture",
            "fine-grained-traffic-flow-control-architecture",
            "incident-management-plan-architecture",
            "layered-network-segmentation-architecture",
            "least-privilege-permission-architecture",
            "secrets-management-architecture",
            "secure-key-management-architecture",
            "secure-third-party-access-architecture",
            "security-design-principles-integration",
            "sensitivity-based-data-protection-architecture",
            "shared-responsibility-model-documentation",
            "software-integrity-validation-architecture",
            "systematic-threat-modeling-in-design",
            "temporary-credentials-architecture",
            "traffic-inspection-architecture",
        ],
    ),
];

/// How many sentences `description` is written in: the full stops that END one, which is a
/// full stop at the end of the text or one followed by a space and a capital letter. A full
/// stop inside an abbreviation or a version (`e.g. `, `v4.0.1 `) is followed by a lower
/// case letter or a digit, so it is not counted as a statement of its own.
fn sentences(description: &str) -> usize {
    description
        .match_indices('.')
        .filter(|(at, _)| {
            let mut rest = description[at + 1..].chars();
            match rest.next() {
                None => true,
                Some(' ') => rest.next().is_some_and(char::is_uppercase),
                Some(_) => false,
            }
        })
        .count()
}

/// RULE: a pack AWS manages reports the COMPLETE requirement set that pack is published
/// with — every name it defines and no name it does not — so a client that reads a managed
/// pack to discover or evaluate against what AWS ships sees the whole framework rather
/// than a sample of it.
///
/// The rows are the four packs the catalog advertises, each asserted over its whole
/// published name set, so an entry dropped from or added to one pack's table fails here
/// whichever pack and whichever name it is.
#[tokio::test]
async fn every_managed_pack_reports_its_complete_published_requirement_set() {
    let router = emulator();

    for (pack_id, published) in PUBLISHED_REQUIREMENT_NAMES {
        let case = format!("{pack_id} published set");
        let listed: Vec<String> = list_all(&router, pack_id, None, &case)
            .await
            .iter()
            .map(|summary| {
                member(summary, "name")
                    .unwrap_or_else(|| panic!("case {case:?}: a summary carries no name"))
            })
            .collect();
        // Sorted, because the operation reports a pack's requirements "in name order"
        // (this crate's OPERATIONS.md) and the cursor resumes at a NAME: the comparison
        // below is therefore an ordering assertion as well as a membership one.
        let mut wanted: Vec<String> = published.iter().map(|name| name.to_string()).collect();
        wanted.sort();

        // The membership check first: `assert_eq!` below already fails on a missing or an
        // extra name, but it reports two long lists, where this names exactly which names
        // are absent and which the pack does not publish.
        let listed_set: BTreeSet<&str> = listed.iter().map(String::as_str).collect();
        let wanted_set: BTreeSet<&str> = published.iter().copied().collect();
        let missing: Vec<&str> = wanted_set.difference(&listed_set).copied().collect();
        let unexpected: Vec<&str> = listed_set.difference(&wanted_set).copied().collect();
        assert!(
            missing.is_empty() && unexpected.is_empty(),
            "case {case:?}: expected the {} published requirements, {} missing \
             ({missing:?}) and {} not published ({unexpected:?}) in the {} reported",
            published.len(),
            missing.len(),
            unexpected.len(),
            listed.len()
        );
        assert_eq!(
            listed, wanted,
            "case {case:?}: expected the published set in name order, got {listed:?}"
        );
    }
}

/// What the published statement of a requirement whose recorded text is quoted SAYS: the
/// phrases that statement is published with, each of which the reported `description` must
/// contain.
///
/// Source: the recorded `description` of each of these requirements on its own step of the
/// `securityagent-catalog-list-security-requirements` fixture, quoted in the report that
/// filed the paraphrases. A row is a pack id, a requirement name and the phrases that
/// requirement's statement states — so a table edit that swaps a statement for other prose
/// of the same shape fails here even though it still passes the form case above.
const PUBLISHED_STATEMENT_PHRASES: [(&str, &str, &[&str]); 5] = [
    (
        "srp-am-nist-csf",
        "adverse-event-analysis-and-threat-intelligence-integration",
        &[
            "Workloads should be designed to support the detection and analysis of \
             anomalies, indicators of compromise (IoCs), and other potentially adverse \
             events.",
            "cyber threat intelligence",
            "establishing defined criteria for escalating adverse events to formal \
             incident declarations",
        ],
    ),
    (
        "srp-am-pci-dss",
        "account-data-classification-and-retention-controls",
        &[
            "cardholder data (CHD)",
            "sensitive authentication data (SAD)",
            "must not be retained after authorization completes, even when it is \
             encrypted",
            "rendered unreadable",
        ],
    ),
    (
        "srp-am-pci-dss",
        "anti-malware-protection-for-workload-components",
        &["protected against malicious software", "anti-malware"],
    ),
    (
        "srp-am-well-architected",
        "authenticated-network-communication-architecture",
        &[
            "east-west",
            "north-south",
            "mutual TLS (mTLS), IPsec, or request signing with a service identity such \
             as SigV4",
        ],
    ),
    (
        "srp-am-well-architected",
        "automated-certificate-management-architecture",
        &["issued, distributed, renewed and revoked automatically"],
    ),
];

/// RULE: a shipped requirement's `description` is that requirement's OWN published
/// statement — what the rule demands, in the words the pack publishes it in — and not a
/// summary of it, because a client renders and evaluates the rule from that member; the
/// framework packs state both what the workload must do and what its design has to
/// demonstrate, which is prose of several sentences, while the base pack's own rules are
/// published as single statements.
///
/// The rows are every requirement of every managed pack, so the form is asserted for the
/// names the issue cited and for every name beside them; the length and sentence bounds
/// are what separates a published statement from the one-line paraphrase a pack must not
/// answer with. The requirements whose recorded statement is quoted are asserted on what
/// that statement SAYS as well, from [`PUBLISHED_STATEMENT_PHRASES`].
#[tokio::test]
async fn a_shipped_requirements_description_is_its_published_statement() {
    let router = emulator();

    for (pack_id, published) in PUBLISHED_REQUIREMENT_NAMES {
        // The base pack holds the service's own default rules, published as one
        // statement each; the other three track a framework whose requirements are
        // published as prose.
        let single_statement = pack_id == "srp-am-base";
        let summaries = list_all(&router, pack_id, None, pack_id).await;

        for name in published {
            let case = format!("{pack_id} / {name}");
            let summary = summaries
                .iter()
                .find(|summary| member(summary, "name").as_deref() == Some(*name))
                .unwrap_or_else(|| panic!("case {case:?}: the listing does not report it"));
            let description = member(summary, "description")
                .unwrap_or_else(|| panic!("case {case:?}: no description was reported"));
            let count = sentences(&description);

            if single_statement {
                assert_eq!(
                    count, 1,
                    "case {case:?}: expected the rule's single published statement, got \
                     {count} sentences: {description:?}"
                );
            } else {
                assert!(
                    count >= 2 && description.len() >= 200,
                    "case {case:?}: expected the requirement's published prose — what the \
                     workload must do and what the design must demonstrate — got {count} \
                     sentence(s) of {} characters: {description:?}",
                    description.len()
                );
            }
            assert!(
                description.trim() == description && description.ends_with('.'),
                "case {case:?}: expected a trimmed, terminated statement, got \
                 {description:?}"
            );

            for (quoted_pack, quoted_name, phrases) in PUBLISHED_STATEMENT_PHRASES {
                if (quoted_pack, quoted_name) != (pack_id, *name) {
                    continue;
                }
                for phrase in phrases {
                    assert!(
                        description.contains(phrase),
                        "case {case:?}: the published statement states {phrase:?}, which \
                         the reported description does not: {description:?}"
                    );
                }
            }
        }
    }
}

/// RULE: reading the shipped content changes nothing — two listings of an untouched
/// account agree member for member, and the catalog table every case above is driven from
/// names each requirement of a pack exactly once with no empty field.
#[tokio::test]
async fn reading_the_shipped_content_does_not_change_it() {
    let router = emulator();

    for pack in AWS_MANAGED_PACKS {
        let case = pack.pack_id;
        let first = list_all(&router, pack.pack_id, None, case).await;
        let names: Vec<&str> = pack.requirements.iter().map(|entry| entry.name).collect();
        batch_get(&router, pack.pack_id, &names, case).await;
        let again = list_all(&router, pack.pack_id, None, case).await;
        assert_eq!(
            again, first,
            "case {case:?}: two reads of the shipped content disagreed"
        );

        let distinct: BTreeSet<&str> = names.iter().copied().collect();
        assert_eq!(
            distinct.len(),
            pack.requirements.len(),
            "case {case:?}: expected {} distinct requirement names in the catalog entry, \
             got {distinct:?}",
            pack.requirements.len()
        );
        for entry in pack.requirements {
            assert!(
                !entry.name.is_empty()
                    && !entry.description.is_empty()
                    && !entry.domain.is_empty()
                    && !entry.evaluation.is_empty(),
                "case {case:?}: the catalog entry {:?} carries an empty required field",
                entry.name
            );
        }
    }
}
