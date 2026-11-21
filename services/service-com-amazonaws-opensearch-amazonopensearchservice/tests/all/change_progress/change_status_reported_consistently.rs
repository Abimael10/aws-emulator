// SPDX-License-Identifier: CC0-1.0

//! What a client that watches a domain's configuration change observes, driven over the
//! real wire path: a `restJson1` request through the multi-service router into the mounted
//! service, so what is asserted is what a polling client sees.
//!
//! ONE rule, over every call that initiates a configuration change on a domain:
//!
//! > a change's status is a property of the CHANGE, not of the call that reports it — so the
//! > answer the call that INITIATED the change gives for it and every read taken after it
//! > name the same change and report it on one progression, never behind the state an
//! > earlier observation already saw the change in: the status moves only FORWARD, and never
//! > back onto a state the service has already left.
//!
//! The axis that is new here is the INITIATING call's own answer measured against the reads
//! that follow it on every path that reports the change: `DescribeDomain`,
//! `DescribeDomains`, `DescribeDomainConfig` and `DescribeDomainChangeProgress`. Agreement
//! among the READ paths is already owned by [`super`] — which asserts their EQUALITY, a
//! stronger relation this suite deliberately does not restate: what is new here is a SEQUENCE
//! that starts at the initiating call and may cross the instant the clock makes the change
//! due, so the relation that holds all along it is "never behind the observation before it" —
//! and the single span each call owes by [`crate::successive_domain_transitions`], which pins
//! each transient value; neither asserts that the call which STARTED the change reports it the
//! same way, which is what a client that reads the status off the create or update answer and
//! then polls depends on: a status that differed between the two would make its first poll look
//! like a step backwards.
//!
//! The second rule is about the one value in that same projection the caller WROTE:
//!
//! > the access policy document a request submits is held and reported back as it was
//! > written — every value inside it, the principal among them, in the form the caller wrote
//! > it — so a client that reads the policy back, edits it and writes it again writes the
//! > document it authored.
//!
//! The harness and the settle poll come from [`crate::domain_lifecycle`]; this suite adds no
//! second copy of them.

use serde_json::{json, Value};

use crate::domain_batch_describe::describe_domains;
use crate::domain_lifecycle::{settled_domain, string_member, Emulator};
// The domain every row provisions, and the instance type it runs on, from the suite that owns
// that shape: the smallest domain this control plane provisions is stated once.
use crate::successive_domain_transitions::{single_node_create, INSTANCE_TYPE};

/// The states a configuration change walks while it is being applied, in the order it walks
/// them, ending in the state a change that has landed is in.
///
/// The spellings are `ConfigChangeStatus`'s own
/// (`smithy_query(query="shape", shape="com.amazonaws.opensearch#ConfigChangeStatus")`
/// publishes Pending / Initializing / Validating / ValidationFailed / ApplyingChanges /
/// Completed / PendingUserInput / Cancelled); the three the enum carries for a change that
/// did NOT land — a validation failure, a change waiting on its caller and a cancelled one —
/// are off this progression and are deliberately not ranked here, so a case that observed
/// one of them fails rather than being silently ranked.
const APPLYING_PROGRESSION: [&str; 5] = [
    "Pending",
    "Initializing",
    "Validating",
    "ApplyingChanges",
    "Completed",
];

/// How far along [`APPLYING_PROGRESSION`] the state `status` is.
fn rank(status: &str, case: &str) -> usize {
    APPLYING_PROGRESSION
        .iter()
        .position(|state| *state == status)
        .unwrap_or_else(|| {
            panic!(
                "case {case}: expected one of the states a change being applied walks \
                 ({APPLYING_PROGRESSION:?}), got {status:?}"
            )
        })
}

/// One call that initiates a configuration change: the name a failure reports it by and the
/// `UpdateDomainConfig` body it is made with (`None` for the create that mints the domain).
///
/// Which state of the progression the change is in when a row's call answers is the
/// SERVICE's to decide — a change waiting for a cluster that is not provisioned yet is not as
/// far along as one being applied to a running one — so no row states a value for it, which is
/// what keeps this table separate from [`crate::successive_domain_transitions`]'s: that one
/// pins each transient value, and what every row asserts here is the rule alone: one change,
/// one status per instant, moving only forward.
struct Initiation {
    case: &'static str,
    update: Option<Value>,
}

/// The rows: the create, then an update that reshapes the cluster and one that only moves a
/// dynamic setting, in the order a client issues them — each on the domain the row before it
/// left idle.
fn initiations() -> Vec<Initiation> {
    vec![
        Initiation {
            case: "the create that mints the domain",
            update: None,
        },
        Initiation {
            case: "an update that scales the data nodes out",
            update: Some(json!({
                "ClusterConfig": {
                    "InstanceType": INSTANCE_TYPE,
                    "InstanceCount": 2,
                    "DedicatedMasterEnabled": false,
                    "ZoneAwarenessEnabled": false,
                },
            })),
        },
        Initiation {
            case: "an update that moves the automated snapshot hour",
            update: Some(json!({ "SnapshotOptions": { "AutomatedSnapshotStartHour": 5 } })),
        },
    ]
}

/// The change a reporting path names, as `(ChangeId, ConfigChangeStatus)`.
fn named_change(details: &Value, case: &str) -> (String, String) {
    (
        string_member(details, "ChangeId", case),
        string_member(details, "ConfigChangeStatus", case),
    )
}

/// What every path that reports a domain's change says about it, each tagged with the
/// operation that said it.
async fn change_as_every_read_reports_it(
    emulator: &Emulator,
    name: &str,
    case: &str,
) -> Vec<(&'static str, (String, String))> {
    let described = emulator.describe(name).await;
    // The batch read and the change read are the suites that own them, reached rather than
    // copied: [`crate::domain_batch_describe::describe_domains`] and [`super::progress`].
    let listed = describe_domains(emulator, &[name]).await;
    let configured = emulator.describe_config(name).await;
    let progress = super::progress(emulator, name, None).await;
    for (reader, answer) in [
        ("DescribeDomain", &described),
        ("DescribeDomains", &listed),
        ("DescribeDomainConfig", &configured),
        ("DescribeDomainChangeProgress", &progress),
    ] {
        assert_eq!(
            answer.status, 200,
            "case {case} ({reader}): expected 200, got {} with {:?}",
            answer.status, answer.body
        );
    }
    vec![
        (
            "DescribeDomain",
            named_change(
                &described.body["DomainStatus"]["ChangeProgressDetails"],
                case,
            ),
        ),
        (
            "DescribeDomains",
            named_change(
                &listed.body["DomainStatusList"][0]["ChangeProgressDetails"],
                case,
            ),
        ),
        (
            "DescribeDomainConfig",
            named_change(
                &configured.body["DomainConfig"]["ChangeProgressDetails"],
                case,
            ),
        ),
        (
            "DescribeDomainChangeProgress",
            named_change(&progress.body["ChangeProgressStatus"], case),
        ),
    ]
}

/// RULE: the call that initiates a configuration change and every read taken after it name
/// the same change and report it on one progression that only moves forward.
#[tokio::test]
async fn every_path_reports_one_change_in_one_status() {
    let emulator = Emulator::new();
    let domain = "one-change-one-status";

    for initiation in initiations() {
        let case = initiation.case;

        // The call that initiates the change, and what IT says about the change it started.
        let initiated = match &initiation.update {
            None => {
                let answer = emulator.create(single_node_create(domain)).await;
                assert_eq!(
                    answer.status, 200,
                    "case {case}: expected 200 from CreateDomain, got {} with {:?}",
                    answer.status, answer.body
                );
                named_change(&answer.body["DomainStatus"]["ChangeProgressDetails"], case)
            }
            Some(body) => {
                let answer = emulator.update_config(domain, body.clone()).await;
                assert_eq!(
                    answer.status, 200,
                    "case {case}: expected 200 from UpdateDomainConfig, got {} with {:?}",
                    answer.status, answer.body
                );
                named_change(&answer.body["DomainConfig"]["ChangeProgressDetails"], case)
            }
        };
        // Every path that reports the change, read straight after that call, names the SAME
        // change and reports it on the same progression, never behind where the observation
        // before it left the change: a client that read the status off the initiating answer
        // and polls sees its first poll continue from there rather than step backwards.
        //
        // Stated as "never behind" rather than "equal to" because the instant the clock makes
        // the change due belongs to the clock: a sequence of observations may cross it, and a
        // change that has landed between two of them has moved FORWARD, which is the rule —
        // whereas any disagreement the rule forbids (two paths reporting different changes,
        // or one reporting a state an earlier observation had already left) fails it, however
        // fast or slow the host runs.
        let mut walked: Vec<(&'static str, (String, String))> =
            vec![("the initiating call", initiated.clone())];
        walked.extend(change_as_every_read_reports_it(&emulator, domain, case).await);
        for ((before, earlier), (reader, reported)) in walked.iter().zip(walked.iter().skip(1)) {
            assert_eq!(
                reported.0, earlier.0,
                "case {case} ({reader} after {before}): expected the change {:?} the \
                 initiating call named, got {:?}",
                earlier.0, reported.0
            );
            assert!(
                rank(&reported.1, case) >= rank(&earlier.1, case),
                "case {case} ({reader} after {before}): a change's status only moves forward, \
                 got {:?} after {:?}",
                reported.1,
                earlier.1
            );
        }

        // Once the span the clock owes the change has passed, it is reported as landed — on
        // every path, still as the same change, and forward of where it started.
        let settled = settled_domain(&emulator, domain).await;
        let settled_change = named_change(&settled["ChangeProgressDetails"], case);
        assert_eq!(
            settled_change.0, initiated.0,
            "case {case}: the change that landed is the one the call started, got \
             {settled_change:?} after {initiated:?}"
        );
        assert_eq!(
            settled_change.1.as_str(),
            "Completed",
            "case {case}: expected the landed change to be reported as completed, got \
             {settled_change:?}"
        );
        // Forward of where the initiating call reported it, which is `>=` rather than `>`:
        // the instant the clock makes the change due belongs to the clock, so a change the
        // initiating call itself already answered as landed has moved forward by nothing —
        // the rule the suite holds the service to is that it never moves BACK.
        assert!(
            rank(&initiated.1, case) <= rank(&settled_change.1, case),
            "case {case}: the landed change is no earlier than where the initiating call \
             reported it, got {} after {}",
            settled_change.1,
            initiated.1
        );
        for (reader, reported) in change_as_every_read_reports_it(&emulator, domain, case).await {
            assert_eq!(
                reported, settled_change,
                "case {case} ({reader}, settled): expected the landed change {settled_change:?}, \
                 got {reported:?}"
            );
        }
    }
}

/// One access policy document a create can carry: what a failure calls it, the document as the
/// request supplies it, and the text its PRINCIPAL is written as, which must survive verbatim.
///
/// The rows differ along the FORM the principal is written in — the bare account id, the
/// account's root user ARN, a wildcard and a role ARN in a one-element list — and along the
/// layout the document arrives in, because what the service holds is the document it was
/// handed rather than a re-rendering of a parse of it. The principal text is stated per row
/// rather than derived, so the assertion on it holds whatever the service's rendering does.
fn submitted_policies() -> Vec<(&'static str, String, String)> {
    let account = ::engine::emulator_account_id();
    vec![
        (
            "a principal written as the bare account id",
            format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"AWS\":\"{account}\"}},\"Action\":\"es:*\",\
                 \"Resource\":\"*\"}}]}}"
            ),
            format!("\"AWS\":\"{account}\""),
        ),
        (
            "a principal written as the account's root user",
            format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"AWS\":\"arn:aws:iam::{account}:root\"}},\
                 \"Action\":\"es:*\",\"Resource\":\"*\"}}]}}"
            ),
            format!("\"AWS\":\"arn:aws:iam::{account}:root\""),
        ),
        (
            "a wildcard principal",
            "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
             \"Principal\":\"*\",\"Action\":[\"es:ESHttpGet\"],\"Resource\":\"*\"}]}"
                .to_string(),
            "\"Principal\":\"*\"".to_string(),
        ),
        (
            "a role principal written as a one-element list, indented",
            format!(
                "{{\n  \"Version\" : \"2012-10-17\",\n  \"Statement\" : [ {{\n    \
                 \"Effect\" : \"Allow\",\n    \"Principal\" : {{\n      \"AWS\" : [ \
                 \"arn:aws:iam::{account}:role/search\" ]\n    }},\n    \"Action\" : \
                 \"es:*\",\n    \"Resource\" : \"*\"\n  }} ]\n}}"
            ),
            format!("\"arn:aws:iam::{account}:role/search\""),
        ),
    ]
}

/// RULE: the access policy document a create submits is reported back as it was written, on
/// every path that reports it and as the value the change in flight is setting it to.
///
/// The expectation is the service's own rendering of a document it was handed — the shared
/// minifier every service that holds one goes through ([`::service_common::json_document`]),
/// which drops the insignificant whitespace BETWEEN the document's JSON tokens and changes
/// nothing else — so what this asserts is that no value inside the document moved.
#[tokio::test]
async fn a_submitted_access_policy_is_echoed_as_it_was_written() {
    let emulator = Emulator::new();

    for (index, (case, submitted, principal)) in submitted_policies().into_iter().enumerate() {
        let domain = format!("policy-echo-{index}");
        let mut body = single_node_create(&domain);
        body["AccessPolicies"] = Value::from(submitted.clone());
        let created = emulator.create(body).await;
        assert_eq!(
            created.status, 200,
            "case {case}: expected 200 from CreateDomain, got {} with {:?}",
            created.status, created.body
        );
        let expected = ::service_common::json_document::minified(&submitted);

        // The create's own answer, the domain read and the configuration read all report the
        // document the request carried.
        let described = emulator.describe(&domain).await;
        let configured = emulator.describe_config(&domain).await;
        for (reader, reported) in [
            (
                "CreateDomain",
                created.body["DomainStatus"]["AccessPolicies"].as_str(),
            ),
            (
                "DescribeDomain",
                described.body["DomainStatus"]["AccessPolicies"].as_str(),
            ),
            (
                "DescribeDomainConfig",
                configured.body["DomainConfig"]["AccessPolicies"]["Options"].as_str(),
            ),
        ] {
            assert_eq!(
                reported,
                Some(expected.as_str()),
                "case {case} ({reader}): expected the document as it was submitted, got \
                 {reported:?}"
            );
            // ... and the principal is in it as the caller wrote it, asserted against the text
            // the row states rather than against anything the service rendered, so a rewrite of
            // that one value fails here whatever the rest of the document does.
            assert!(
                reported.is_some_and(|document| document.contains(&principal)),
                "case {case} ({reader}): expected the principal {principal:?} as submitted, got \
                 {reported:?}"
            );
            // ... and the document DENOTES the one submitted, member for member, read with no
            // help from the service's own rendering of it: every value inside survived, not only
            // the principal this report is about.
            assert_eq!(
                reported.and_then(|document| ::serde_json::from_str::<Value>(document).ok()),
                ::serde_json::from_str::<Value>(&submitted).ok(),
                "case {case} ({reader}): expected the document the request submitted, got \
                 {reported:?}"
            );
        }

        // And so does the entry the change in flight reports for it: the value the policy is
        // being set to is the document the caller wrote, JSON content whoever typed it.
        let modifying = created.body["DomainStatus"]["ModifyingProperties"]
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: expected ModifyingProperties, got {:?}",
                    created.body["DomainStatus"]
                )
            })
            .clone();
        let entry = modifying
            .iter()
            .find(|entry| entry["Name"].as_str() == Some("AccessPolicies"))
            .unwrap_or_else(|| {
                panic!("case {case}: expected an AccessPolicies entry among {modifying:?}")
            });
        assert_eq!(
            (entry["PendingValue"].as_str(), entry["ValueType"].as_str()),
            (Some(expected.as_str()), Some("STRINGIFIED_JSON")),
            "case {case}: expected the submitted document as the value the policy is moving \
             to, got {entry:?}"
        );
    }
}
