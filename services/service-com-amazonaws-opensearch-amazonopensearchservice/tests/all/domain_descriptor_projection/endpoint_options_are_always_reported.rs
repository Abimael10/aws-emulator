// SPDX-License-Identifier: CC0-1.0

//! What a descriptor of a domain reports for the options of its ENDPOINT, driven over
//! the real wire path — a `restJson1` request through the multi-service router into the
//! mounted service — so what is asserted is what a client sees.
//!
//! ONE rule, over every projection of a stored domain that carries the container:
//!
//! > the service provisions a domain with the EFFECTIVE options of its endpoint, so
//! > every descriptor of a live domain carries the `DomainEndpointOptions` structure
//! > whether or not the request that created or changed the domain supplied one — and a
//! > domain with no custom endpoint reports `CustomEndpointEnabled` as the boolean
//! > `false`, with neither `CustomEndpoint` nor `CustomEndpointCertificateArn`, so a
//! > client reading the container unconditionally can tell "no custom endpoint" from
//! > "the service said nothing".
//!
//! So the assertions span the inputs the rule generalises over: how much of the
//! container the request named — nothing at all, an empty container, one member of it,
//! two members, and the custom-endpoint toggle sent explicitly off — and WHICH
//! projection is read: the `DomainStatus` of `CreateDomain`, of `DescribeDomain`, of the
//! batch `DescribeDomains` and of `DeleteDomain`, the `DryRunConfig` a pre-update
//! analysis is reported with, and the `DomainConfig` container `DescribeDomainConfig`
//! answers — plus a domain the container was introduced on by an UPDATE rather than by
//! the create.
//!
//! The harness, the baseline `CreateDomain` body and the poll helpers come from
//! [`crate::domain_lifecycle`], the batch read from [`crate::domain_batch_describe`] and
//! the published TLS policy value set from [`crate::domain_input_validation`]; this suite
//! adds no second copy of any of them.

use serde_json::{json, Value};

use crate::domain_batch_describe::describe_domains as batch_describe;
use crate::domain_input_validation::PUBLISHED_TLS_POLICIES;
use crate::domain_lifecycle::{baseline_create, settled_domain, Emulator, PREFIX};

/// One row: how much of the `DomainEndpointOptions` container the request names, and
/// `None` for a request that names the member not at all — the shape of the create the
/// rule is read on most often, since the member is optional on the input.
struct Row {
    case: &'static str,
    sent: Option<Value>,
}

/// The rows, differing along how much of the container the request fills in and which
/// members it fills. The TLS policies named are values the model publishes for
/// `TLSSecurityPolicy` ([`PUBLISHED_TLS_POLICIES`]) other than the one the service
/// settles a domain that names none at, so a row's own value is visibly kept.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no container at all",
            sent: None,
        },
        Row {
            case: "an empty container",
            sent: Some(json!({})),
        },
        Row {
            case: "HTTPS required",
            sent: Some(json!({ "EnforceHTTPS": true })),
        },
        Row {
            case: "the oldest published TLS policy",
            sent: Some(json!({ "TLSSecurityPolicy": "Policy-Min-TLS-1-0-2019-07" })),
        },
        Row {
            case: "HTTPS required under a perfect-forward-secrecy TLS policy",
            sent: Some(json!({
                "EnforceHTTPS": true,
                "TLSSecurityPolicy": "Policy-Min-TLS-1-2-PFS-2023-10",
            })),
        },
        Row {
            case: "the custom endpoint toggle sent off",
            sent: Some(json!({ "CustomEndpointEnabled": false })),
        },
        Row {
            case: "HTTPS not required and the custom endpoint toggle off",
            sent: Some(json!({ "EnforceHTTPS": false, "CustomEndpointEnabled": false })),
        },
    ]
}

/// Assert `reported` is the effective endpoint options of a domain that has no custom
/// endpoint, and that every member `sent` named is reported as it was sent.
///
/// The members the request did NOT name are the service's own, so what is asserted of
/// them is what a client can rely on: the custom-endpoint toggle is the boolean `false`
/// (not an absent member), no custom endpoint or certificate is reported for an endpoint
/// that has none, whether HTTPS is required is answered as a boolean either way, and the
/// policy the HTTPS endpoint applies is one of the published values.
fn assert_effective(reported: &Value, sent: Option<&Value>, case: &str) {
    let held = reported.as_object().unwrap_or_else(|| {
        panic!("case {case}: expected the DomainEndpointOptions structure, got {reported:?}")
    });
    assert_eq!(
        held.get("CustomEndpointEnabled"),
        Some(&json!(false)),
        "case {case}: expected CustomEndpointEnabled reported as false, got {:?}",
        held.get("CustomEndpointEnabled")
    );
    for member in ["CustomEndpoint", "CustomEndpointCertificateArn"] {
        assert_eq!(
            held.get(member),
            None,
            "case {case}: a domain with no custom endpoint reports no {member}, got {:?}",
            held.get(member)
        );
    }
    assert!(
        held.get("EnforceHTTPS").is_some_and(Value::is_boolean),
        "case {case}: expected EnforceHTTPS answered as a boolean, got {:?}",
        held.get("EnforceHTTPS")
    );
    let policy = held
        .get("TLSSecurityPolicy")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case}: expected a TLSSecurityPolicy, got {reported:?}")
        });
    assert!(
        PUBLISHED_TLS_POLICIES.contains(&policy),
        "case {case}: expected one of the published TLS policies \
         {PUBLISHED_TLS_POLICIES:?}, got {policy:?}"
    );
    for (member, value) in sent.and_then(Value::as_object).into_iter().flatten() {
        assert_eq!(
            held.get(member),
            Some(value),
            "case {case}: expected {member} reported as sent ({value}), got {:?}",
            held.get(member)
        );
    }
}

/// `CreateDomain` for `name` with the baseline body plus `sent` as its
/// `DomainEndpointOptions` — and with the member left out altogether where `sent` is
/// `None` — asserting it was accepted, and the `DomainStatus` it answered with.
async fn created(emulator: &Emulator, name: &str, sent: Option<&Value>, case: &str) -> Value {
    let mut body = baseline_create(name);
    if let Some(sent) = sent {
        body["DomainEndpointOptions"] = sent.clone();
    }
    let answer = emulator.create(body.clone()).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: CreateDomain({body}) expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["DomainStatus"].clone()
}

/// How many domains one `DescribeDomains` call may name: the service refuses a request
/// naming more ("Please provide a maximum of 5 domain names to describe."), so a table
/// longer than that is read in several calls.
const BATCH_LIMIT: usize = 5;

/// The `DomainStatus` list `DescribeDomains` reports for `names`, in the order asked for
/// — read in batches of at most [`BATCH_LIMIT`] names.
async fn describe_domains(emulator: &Emulator, names: &[String]) -> Vec<Value> {
    let mut reported = Vec::new();
    for batch in names.chunks(BATCH_LIMIT) {
        let batch: Vec<&str> = batch.iter().map(String::as_str).collect();
        let answer = batch_describe(emulator, &batch).await;
        assert_eq!(
            answer.status, 200,
            "case DescribeDomains({batch:?}): expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        let listed = answer.body["DomainStatusList"]
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case DescribeDomains({batch:?}): expected a DomainStatusList, got {:?}",
                    answer.body
                )
            })
            .clone();
        assert_eq!(
            listed.len(),
            batch.len(),
            "case DescribeDomains({batch:?}): expected one status per domain, got {listed:?}"
        );
        reported.extend(listed);
    }
    reported
}

/// The `DomainEndpointOptions` container `DescribeDomainConfig` reports for `name`.
async fn configured(emulator: &Emulator, name: &str, case: &str) -> Value {
    let answer = emulator.describe_config(name).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: DescribeDomainConfig({name:?}) expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["DomainConfig"]["DomainEndpointOptions"]["Options"].clone()
}

#[tokio::test]
async fn every_descriptor_of_a_domain_reports_its_effective_endpoint_options() {
    let emulator = Emulator::new();
    let rows = rows();
    let mut names = Vec::new();

    for (index, row) in rows.iter().enumerate() {
        let name = format!("endpoint-options-{index}");
        let case = format!("CreateDomain({}, {})", name, row.case);
        let status = created(&emulator, &name, row.sent.as_ref(), &case).await;
        assert_effective(&status["DomainEndpointOptions"], row.sent.as_ref(), &case);
        names.push(name);
    }

    // The single-domain reader, the batch reader and the configuration reader report the
    // one stored container — the batch call naming every domain at once.
    let listed = describe_domains(&emulator, &names).await;
    for ((row, name), batched) in rows.iter().zip(names.iter()).zip(listed.iter()) {
        let described = emulator.describe(name).await;
        assert_eq!(
            described.status, 200,
            "case DescribeDomain({name:?}): expected 200, got {} with {:?}",
            described.status, described.body
        );
        let reported = described.body["DomainStatus"]["DomainEndpointOptions"].clone();
        assert_effective(
            &reported,
            row.sent.as_ref(),
            &format!("DescribeDomain({}, {})", name, row.case),
        );
        assert_eq!(
            &batched["DomainEndpointOptions"], &reported,
            "case DescribeDomains({}, {}): expected what DescribeDomain reports ({reported}), \
             got {:?}",
            name, row.case, batched["DomainEndpointOptions"]
        );
        assert_eq!(
            configured(&emulator, name, &format!("DescribeDomainConfig({name:?})")).await,
            reported,
            "case DescribeDomainConfig({}, {}): expected the one stored container",
            name,
            row.case
        );

        // The status a domain is torn down with reports the container it was holding.
        let deleted = emulator.delete(name).await;
        assert_eq!(
            deleted.status, 200,
            "case DeleteDomain({name:?}): expected 200, got {} with {:?}",
            deleted.status, deleted.body
        );
        assert_eq!(
            deleted.body["DomainStatus"]["DomainEndpointOptions"], reported,
            "case DeleteDomain({}, {}): expected the container the domain was holding \
             ({reported}), got {:?}",
            name, row.case, deleted.body["DomainStatus"]["DomainEndpointOptions"]
        );
    }
}

#[tokio::test]
async fn a_container_introduced_by_an_update_is_completed_the_same_way() {
    let emulator = Emulator::new();
    // The rows a request can CHANGE the container with: one member, the other member,
    // and both — each applied to a domain created with no container at all, so what the
    // update leaves is the effective container rather than only what it named.
    let changes: Vec<(&str, Value)> = vec![
        ("HTTPS required", json!({ "EnforceHTTPS": true })),
        (
            "another published TLS policy",
            json!({ "TLSSecurityPolicy": "Policy-Min-TLS-1-2-RFC9151-FIPS-2024-08" }),
        ),
        (
            "both members at once",
            json!({
                "EnforceHTTPS": false,
                "TLSSecurityPolicy": "Policy-Min-TLS-1-0-2019-07",
            }),
        ),
    ];

    // Every domain is created first and settled afterwards, so the provisioning spans run
    // alongside each other: a change is applied to a SETTLED domain, and waiting for that
    // condition is what each poll below does.
    let mut names = Vec::new();
    for (index, (case, _)) in changes.iter().enumerate() {
        let name = format!("endpoint-options-update-{index}");
        created(
            &emulator,
            &name,
            None,
            &format!("CreateDomain({}, {})", name, case),
        )
        .await;
        names.push(name);
    }
    for name in &names {
        settled_domain(&emulator, name).await;
    }

    for ((case, sent), name) in changes.iter().zip(names.iter()) {
        let answer = emulator
            .update_config(name, json!({ "DomainEndpointOptions": sent.clone() }))
            .await;
        assert_eq!(
            answer.status, 200,
            "case UpdateDomainConfig({name:?}, {case}): expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        assert_effective(
            &answer.body["DomainConfig"]["DomainEndpointOptions"]["Options"],
            Some(sent),
            &format!("UpdateDomainConfig({name:?}, {case})"),
        );
    }
    for ((case, sent), name) in changes.iter().zip(names.iter()) {
        let status = settled_domain(&emulator, name).await;
        assert_effective(
            &status["DomainEndpointOptions"],
            Some(sent),
            &format!("DescribeDomain({name:?}, {case})"),
        );
        assert_effective(
            &configured(&emulator, name, &format!("DescribeDomainConfig({name:?})")).await,
            Some(sent),
            &format!("DescribeDomainConfig({name:?}, {case})"),
        );
    }
}

#[tokio::test]
async fn the_planned_configuration_of_a_dry_run_reports_them_too() {
    let emulator = Emulator::new();
    // Two domains, one created with no container at all and one with part of it: a dry
    // run reports the configuration the domain WOULD hold, which carries the effective
    // endpoint options of the domain the change is planned against.
    let planned: Vec<(&str, Option<Value>)> = vec![
        ("no container at all", None),
        (
            "HTTPS required",
            Some(json!({ "EnforceHTTPS": true })),
        ),
    ];

    let mut names = Vec::new();
    for (index, (case, sent)) in planned.iter().enumerate() {
        let name = format!("endpoint-options-dry-run-{index}");
        created(
            &emulator,
            &name,
            sent.as_ref(),
            &format!("CreateDomain({}, {})", name, case),
        )
        .await;
        names.push(name);
    }
    // A verbose dry run runs its check against the live domain, so the domain it is asked
    // of has to have settled first.
    for name in &names {
        settled_domain(&emulator, name).await;
    }

    for ((case, sent), name) in planned.iter().zip(names.iter()) {
        // A change to another option entirely: what is read back is the endpoint options
        // the planned configuration carries, which this change does not name.
        let answer = emulator
            .update_config(
                name,
                json!({
                    "SnapshotOptions": { "AutomatedSnapshotStartHour": 3 },
                    "DryRun": true,
                    "DryRunMode": "Verbose",
                }),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case UpdateDomainConfig(DryRun, {name:?}, {case}): expected 200, got {} with {:?}",
            answer.status, answer.body
        );

        let progress = emulator
            .call(
                "GET",
                &format!("{PREFIX}/opensearch/domain/{name}/dryRun?loadDryRunConfig=true"),
                None,
            )
            .await;
        assert_eq!(
            progress.status, 200,
            "case DescribeDryRunProgress({name:?}, {case}): expected 200, got {} with {:?}",
            progress.status, progress.body
        );
        assert_effective(
            &progress.body["DryRunConfig"]["DomainEndpointOptions"],
            sent.as_ref(),
            &format!("DescribeDryRunProgress({name:?}, {case})"),
        );
    }
}
