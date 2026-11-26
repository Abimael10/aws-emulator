// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the routes of this API the on-disk model does not describe: a
//! domain's cross-cluster REPLICATION family (`DescribeDomainReplication`,
//! `AddReplicationPeer`, `RemoveReplicationPeer`, `FailoverReplication`,
//! `SwitchoverReplication`) and `CompareDomainConfiguration`.
//!
//! Every case drives the real wire path — a signed `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The harness is this crate's own
//! ([`super::domain_lifecycle`] for the control plane, [`super::domain_rest_data_plane`] for
//! the domain's own REST surface); this suite adds no second copy of either.
//!
//! The RULES under test, each asserted over a SET of inputs rather than over one recorded
//! call:
//!
//! * a call of this family is answered by the operation it addresses, in that operation's own
//!   refusal class — the unknown-domain class, the not-replicating class and the
//!   constraint class each worded once — never with the front door's unknown-operation
//!   answer;
//! * an arrangement is state: establishing one is visible on BOTH domains through the read,
//!   a promotion moves the roles on both, and releasing it leaves both exactly as a domain
//!   that never had one;
//! * validation precedes the effect: a refused `AddReplicationPeer` establishes nothing,
//!   whichever way its dry-run member is set, and a dry run that passes validation
//!   establishes nothing either;
//! * a comparison counts the configuration the two domains HOLD, per category, with its
//!   ARN-format and category validation running before either side is resolved.
//!
//! No case waits for the clock: none of these routes is gated on a domain's status, so the
//! domains are created and used.

use serde_json::{json, Value};

use super::domain_lifecycle::{error_code, Answer, Emulator, PREFIX, RECORDED_NOT_FOUND_STATUS};
use super::domain_rest_data_plane::rest_prefixed;

/// The error code every refused input of this service travels under.
const VALIDATION: &str = "ValidationException";

/// The error code its missing-resource refusals travel under.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The roles a domain plays in an arrangement, as the read reports them.
const LEADER: &str = "LEADER";
/// See [`LEADER`].
const FOLLOWER: &str = "FOLLOWER";

/// The categories `CompareDomainConfiguration` supports, in the order the recorded refusal
/// of an unsupported one lists them.
///
/// Written out here rather than read off the implementation, because the list AND its order
/// are the fixed text of that recorded message: the expectation is the recorded wording, not
/// whatever the code currently carries.
const SUPPORTED_CATEGORIES: [&str; 11] = [
    "cluster_settings",
    "security_role_mappings",
    "ism_policies",
    "security_internal_users",
    "aliases",
    "alerting_monitors",
    "templates",
    "component_templates",
    "security_roles",
    "security_action_groups",
    "ingest_pipelines",
];

/// `GET /2021-01-01/opensearch/domain/{DomainName}/replication`.
async fn describe_replication(emulator: &Emulator, domain: &str) -> Answer {
    emulator
        .call("GET", &format!("{PREFIX}/opensearch/domain/{domain}/replication"), None)
        .await
}

/// One `POST` of a domain's replication plane: `addPeer`, `removePeer`, `failover` or
/// `switchover`.
async fn replication_action(
    emulator: &Emulator,
    domain: &str,
    action: &str,
    body: Value,
) -> Answer {
    emulator
        .call(
            "POST",
            &format!("{PREFIX}/opensearch/domain/{domain}/replication/{action}"),
            Some(body),
        )
        .await
}

/// `POST /2021-01-01/opensearch/compareDomainConfiguration`.
async fn compare(emulator: &Emulator, body: Value) -> Answer {
    emulator
        .call(
            "POST",
            &format!("{PREFIX}/opensearch/compareDomainConfiguration"),
            Some(body),
        )
        .await
}

/// Create a domain and hand back the ARN its own `DomainStatus` reports, so no case composes
/// an ARN by hand.
async fn given_domain(emulator: &Emulator, name: &str) -> String {
    let created = emulator.create(json!({ "DomainName": name })).await;
    assert_eq!(
        created.status, 200,
        "case CreateDomain({name:?}): expected 200, got {} with {:?}",
        created.status, created.body
    );
    created.body["DomainStatus"]["ARN"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case CreateDomain({name:?}): expected an ARN, got {:?}",
                created.body
            )
        })
        .to_string()
}

/// The ARN a domain of this account and Region carries, derived from one the CONTROL PLANE
/// itself reported for a domain that exists.
///
/// So the partition, the Region and the account id of an expectation are the service's own
/// rather than values written into this suite, and the only part this suite chooses is the
/// name — which is what the request carries.
fn arn_like(reported: &str, name: &str) -> String {
    let (prefix, _) = reported
        .rsplit_once('/')
        .unwrap_or_else(|| panic!("expected a domain ARN with a resource name, got {reported:?}"));
    format!("{prefix}/{name}")
}

/// The same ARN with one of its colon-separated fields replaced — how a case composes an ARN
/// that is WELL FORMED and names no domain of this account and Region, out of one the
/// control plane reported.
fn arn_with_field(reported: &str, field: usize, value: &str) -> String {
    let mut fields: Vec<String> = reported.split(':').map(str::to_string).collect();
    if let Some(held) = fields.get_mut(field) {
        *held = value.to_string();
    }
    fields.join(":")
}

/// What this service says of a name that names no stored domain.
fn not_found_message(arn: &str) -> String {
    format!("Domain not found: {arn}")
}

/// What it says of a domain that replicates with nobody — the subject is the CALLING
/// domain's own ARN.
fn not_replicating_message(arn: &str) -> String {
    format!("Domain is not replicating: {arn}")
}

/// The peers a read reports, as `(peer ARN, the peer's role)` pairs.
fn reported_peers(answer: &Answer, case: &str) -> Vec<(String, String)> {
    let peers = answer.body["Peers"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: expected a Peers array, got {:?}", answer.body));
    peers
        .iter()
        .map(|peer| {
            (
                peer["PeerDomainArn"].as_str().unwrap_or_default().to_string(),
                peer["Role"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// RULE: every call of the replication family on a domain this account does not hold is
/// answered by the operation it addresses, in THAT operation's own refusal class — the
/// unknown-domain classes differ per route and the message's subject is the domain's own ARN
/// — rather than with the front door's unknown-operation answer.
///
/// The rows span the five routes and, within each, three names nobody created that differ in
/// what the rule generalises over: length, digits and hyphens.
#[tokio::test]
async fn every_replication_call_on_an_unheld_domain_is_refused_in_its_own_class() {
    let emulator = Emulator::new();
    // One domain that DOES exist, for the ARN the expectations are derived from and as a
    // resolvable peer for the rows that name one.
    let peer_arn = given_domain(&emulator, "held-peer").await;

    for absent in ["abc", "a-very-long-absent-domain-x", "absent9domain"] {
        let arn = arn_like(&peer_arn, absent);
        // (case, the answer, the status, the code, the message)
        let rows: Vec<(String, Answer, u16, &str, String)> = vec![
            (
                format!("DescribeDomainReplication({absent})"),
                describe_replication(&emulator, absent).await,
                RECORDED_NOT_FOUND_STATUS,
                NOT_FOUND,
                not_found_message(&arn),
            ),
            (
                format!("AddReplicationPeer({absent})"),
                replication_action(
                    &emulator,
                    absent,
                    "addPeer",
                    json!({ "PeerDomainArn": peer_arn }),
                )
                .await,
                400,
                VALIDATION,
                not_found_message(&arn),
            ),
            (
                format!("RemoveReplicationPeer({absent})"),
                replication_action(
                    &emulator,
                    absent,
                    "removePeer",
                    json!({ "PeerDomainArn": peer_arn }),
                )
                .await,
                RECORDED_NOT_FOUND_STATUS,
                NOT_FOUND,
                not_replicating_message(&arn),
            ),
            (
                format!("FailoverReplication({absent})"),
                replication_action(&emulator, absent, "failover", json!({ "AllowDataLoss": true }))
                    .await,
                RECORDED_NOT_FOUND_STATUS,
                NOT_FOUND,
                not_found_message(&arn),
            ),
            (
                format!("SwitchoverReplication({absent})"),
                replication_action(&emulator, absent, "switchover", json!({})).await,
                RECORDED_NOT_FOUND_STATUS,
                NOT_FOUND,
                not_found_message(&arn),
            ),
        ];

        for (case, answer, status, code, message) in rows {
            assert_eq!(
                (answer.status, error_code(&answer).as_str()),
                (status, code),
                "case {case}: expected {status} {code}, got {} with {:?}",
                answer.status,
                answer.body
            );
            assert_eq!(
                answer.body["message"].as_str(),
                Some(message.as_str()),
                "case {case}: expected {message:?}, got {:?}",
                answer.body
            );
        }
    }
}

/// RULE: a domain that replicates with nobody READS as one — `Peers` present and empty, and
/// no role at all — and every call that acts on an existing arrangement is refused in its own
/// class naming that domain's own ARN, leaving the domain untouched.
#[tokio::test]
async fn a_domain_that_replicates_with_nobody_reports_no_peers_and_refuses_the_rest() {
    let emulator = Emulator::new();
    let name = "unreplicated-one";
    let arn = given_domain(&emulator, name).await;

    let read = describe_replication(&emulator, name).await;
    assert_eq!(
        read.status, 200,
        "case DescribeDomainReplication({name}): expected 200, got {} with {:?}",
        read.status, read.body
    );
    assert_eq!(
        reported_peers(&read, "an unreplicated domain"),
        Vec::new(),
        "case DescribeDomainReplication({name}): expected no peers, got {:?}",
        read.body
    );
    assert!(
        read.body.get("Role").is_none(),
        "case DescribeDomainReplication({name}): expected no Role member at all, got {:?}",
        read.body
    );

    // (case, the action, its body, the status, the code)
    let rows: Vec<(&str, &str, Value, u16, &str)> = vec![
        (
            "RemoveReplicationPeer on a domain with no arrangement",
            "removePeer",
            json!({ "PeerDomainArn": arn }),
            RECORDED_NOT_FOUND_STATUS,
            NOT_FOUND,
        ),
        (
            "FailoverReplication on a domain with no arrangement",
            "failover",
            json!({ "AllowDataLoss": true }),
            400,
            VALIDATION,
        ),
        (
            "SwitchoverReplication on a domain with no arrangement",
            "switchover",
            json!({}),
            400,
            VALIDATION,
        ),
    ];
    for (case, action, body, status, code) in rows {
        let answer = replication_action(&emulator, name, action, body).await;
        assert_eq!(
            (answer.status, error_code(&answer).as_str()),
            (status, code),
            "case {case}: expected {status} {code}, got {} with {:?}",
            answer.status,
            answer.body
        );
        assert_eq!(
            answer.body["message"].as_str(),
            Some(not_replicating_message(&arn).as_str()),
            "case {case}: expected the not-replicating refusal naming {arn}, got {:?}",
            answer.body
        );
    }

    // Nothing of the domain moved: a refused call on its replication plane is not a change
    // to the domain.
    let described = emulator.describe(name).await;
    assert_eq!(
        (
            described.status,
            described.body["DomainStatus"]["Deleted"].as_bool()
        ),
        (200, Some(false)),
        "case DescribeDomain({name}) after the refused calls: expected the domain intact, got \
         {:?}",
        described.body
    );
}

/// RULE: an arrangement is STATE. `AddReplicationPeer` writes both sides of it — the domain
/// that asked follows its peer and the peer leads — a promotion moves the roles on both
/// records, and `RemoveReplicationPeer` leaves both domains exactly as one that never had an
/// arrangement.
///
/// The rows the promotion is asserted over are the two calls that perform it, each on a fresh
/// arrangement, because the recorded refusals show them taking the same gates.
#[tokio::test]
async fn an_arrangement_is_written_on_both_domains_and_released_from_both() {
    let emulator = Emulator::new();
    let follower = "following-domain";
    let leader = "leading-domain";
    let follower_arn = given_domain(&emulator, follower).await;
    let leader_arn = given_domain(&emulator, leader).await;

    for promotion in ["failover", "switchover"] {
        let established = replication_action(
            &emulator,
            follower,
            "addPeer",
            json!({ "PeerDomainArn": leader_arn }),
        )
        .await;
        assert_eq!(
            established.status, 200,
            "case AddReplicationPeer({follower} -> {leader}) before {promotion}: expected 200, \
             got {} with {:?}",
            established.status, established.body
        );

        // Both sides, read back through the read: the domain that asked is the follower of
        // its peer, and the peer leads.
        for (case, domain, role, peer, peer_role) in [
            (
                format!("the domain that asked, before {promotion}"),
                follower,
                FOLLOWER,
                leader_arn.clone(),
                LEADER,
            ),
            (
                format!("its peer, before {promotion}"),
                leader,
                LEADER,
                follower_arn.clone(),
                FOLLOWER,
            ),
        ] {
            let read = describe_replication(&emulator, domain).await;
            assert_eq!(
                (read.status, read.body["Role"].as_str()),
                (200, Some(role)),
                "case {case}: expected 200 and role {role}, got {} with {:?}",
                read.status,
                read.body
            );
            assert_eq!(
                reported_peers(&read, &case),
                vec![(peer.clone(), peer_role.to_string())],
                "case {case}: expected the one peer {peer} as {peer_role}, got {:?}",
                read.body
            );
        }

        // The promotion: the domain it is called on leads, and its peer follows — on both
        // records.
        let promoted = replication_action(&emulator, follower, promotion, json!({})).await;
        assert_eq!(
            promoted.status, 200,
            "case {promotion}({follower}): expected 200, got {} with {:?}",
            promoted.status, promoted.body
        );
        for (case, domain, role) in [
            (format!("the promoted domain after {promotion}"), follower, LEADER),
            (format!("its peer after {promotion}"), leader, FOLLOWER),
        ] {
            let read = describe_replication(&emulator, domain).await;
            assert_eq!(
                read.body["Role"].as_str(),
                Some(role),
                "case {case}: expected role {role}, got {:?}",
                read.body
            );
        }

        // Releasing it takes BOTH sides with it.
        let released = replication_action(
            &emulator,
            follower,
            "removePeer",
            json!({ "PeerDomainArn": leader_arn }),
        )
        .await;
        assert_eq!(
            released.status, 200,
            "case RemoveReplicationPeer({follower} -> {leader}) after {promotion}: expected 200, \
             got {} with {:?}",
            released.status, released.body
        );
        for domain in [follower, leader] {
            let read = describe_replication(&emulator, domain).await;
            let case = format!("{domain} after the release following {promotion}");
            assert_eq!(
                reported_peers(&read, &case),
                Vec::new(),
                "case {case}: expected no peers, got {:?}",
                read.body
            );
            assert!(
                read.body.get("Role").is_none(),
                "case {case}: expected no Role member at all, got {:?}",
                read.body
            );
        }
    }
}

/// RULE: `AddReplicationPeer` validates BEFORE it establishes anything, and refuses
/// identically whichever way its dry-run member is set: a peer that names no domain of this
/// account is refused with the unknown-domain class naming the ARN exactly as SENT, and a dry
/// run that passes validation establishes nothing either.
///
/// The rows span the dry-run member's three forms (absent, false, true) and three peer values
/// that name no domain: one in the Region the request is signed for, one in another Region,
/// and one in another account.
#[tokio::test]
async fn add_peer_refuses_an_unresolvable_peer_alike_with_and_without_a_dry_run() {
    let emulator = Emulator::new();
    let local = "validating-domain";
    let local_arn = given_domain(&emulator, local).await;
    let peer_name = "resolvable-peer";
    let peer_arn = given_domain(&emulator, peer_name).await;

    let unresolvable = [
        // A name nobody created, in the Region the request is signed for.
        arn_like(&local_arn, "never-created-domain"),
        // An existing name in ANOTHER Region, which is another domain.
        arn_with_field(&local_arn, 3, "eu-west-1"),
        // And in another account.
        arn_with_field(&local_arn, 4, "123456789012"),
    ];
    for sent in &unresolvable {
        for dry_run in [None, Some(false), Some(true)] {
            let mut body = json!({ "PeerDomainArn": sent });
            if let Some(dry_run) = dry_run {
                body["DryRun"] = json!(dry_run);
            }
            let case = format!("AddReplicationPeer(peer {sent}, DryRun {dry_run:?})");
            let answer = replication_action(&emulator, local, "addPeer", body).await;
            assert_eq!(
                (answer.status, error_code(&answer).as_str()),
                (400, VALIDATION),
                "case {case}: expected 400 {VALIDATION}, got {} with {:?}",
                answer.status,
                answer.body
            );
            assert_eq!(
                answer.body["message"].as_str(),
                Some(not_found_message(sent).as_str()),
                "case {case}: expected the peer ARN as sent in the message, got {:?}",
                answer.body
            );
        }
    }

    // A request naming no peer at all is refused for the member it omits, by the wording
    // every absent required member of this service earns.
    let omitted = replication_action(&emulator, local, "addPeer", json!({})).await;
    assert_eq!(
        (omitted.status, error_code(&omitted).as_str()),
        (400, VALIDATION),
        "case AddReplicationPeer(no PeerDomainArn): expected 400 {VALIDATION}, got {} with {:?}",
        omitted.status,
        omitted.body
    );
    assert_eq!(
        omitted.body["message"].as_str(),
        Some(
            "1 validation error detected: Value null at 'peerDomainArn' failed to satisfy \
             constraint: Member must not be null"
        ),
        "case AddReplicationPeer(no PeerDomainArn): got {:?}",
        omitted.body
    );

    // A domain cannot be its own peer: an arrangement has two sides.
    let itself = replication_action(
        &emulator,
        local,
        "addPeer",
        json!({ "PeerDomainArn": local_arn }),
    )
    .await;
    assert_eq!(
        (itself.status, error_code(&itself).as_str()),
        (400, VALIDATION),
        "case AddReplicationPeer(its own ARN): expected 400 {VALIDATION}, got {} with {:?}",
        itself.status,
        itself.body
    );

    // Nothing above established anything.
    let read = describe_replication(&emulator, local).await;
    assert_eq!(
        reported_peers(&read, "after every refused add"),
        Vec::new(),
        "case DescribeDomainReplication({local}): expected no peer after the refused calls, got \
         {:?}",
        read.body
    );

    // A DRY RUN that passes validation establishes nothing either, and the live call that
    // follows it does.
    let dry = replication_action(
        &emulator,
        local,
        "addPeer",
        json!({ "PeerDomainArn": peer_arn, "DryRun": true }),
    )
    .await;
    assert_eq!(
        dry.status, 200,
        "case AddReplicationPeer(a resolvable peer, DryRun true): expected 200, got {} with {:?}",
        dry.status, dry.body
    );
    for (case, domain) in [("the domain that asked", local), ("its peer", peer_name)] {
        let read = describe_replication(&emulator, domain).await;
        assert_eq!(
            reported_peers(&read, case),
            Vec::new(),
            "case {case} after a dry run: expected no peer, got {:?}",
            read.body
        );
    }
    let live = replication_action(
        &emulator,
        local,
        "addPeer",
        json!({ "PeerDomainArn": peer_arn, "DryRun": false }),
    )
    .await;
    assert_eq!(
        live.status, 200,
        "case AddReplicationPeer(a resolvable peer, DryRun false): expected 200, got {} with {:?}",
        live.status, live.body
    );
    let read = describe_replication(&emulator, local).await;
    assert_eq!(
        reported_peers(&read, "after the live add"),
        vec![(peer_arn.clone(), LEADER.to_string())],
        "case DescribeDomainReplication({local}): expected the peer the live call established, \
         got {:?}",
        read.body
    );
}

/// RULE: the `DomainName` one of these paths carries is held to the constraints the MODEL
/// declares for a domain name, worded as every other constraint refusal of this service is —
/// so a malformed name is a client error rather than a missing resource.
///
/// The rows are the published bounds of `DomainName` (`@length(3, 28)`,
/// `@pattern("^[a-z][a-z0-9\-]+$")`, from
/// `smithy_query(query="shape",shape="DomainName")`): each bound, one value past it, and the
/// shapes the pattern refuses.
#[tokio::test]
async fn the_domain_name_in_the_path_is_held_to_the_models_constraints() {
    let emulator = Emulator::new();
    let held = given_domain(&emulator, "constraint-probe").await;

    let twenty_nine = "a".repeat(29);
    let twenty_eight = "a".repeat(28);
    // (the name sent, the clause the refusal states, or None for a name the model admits)
    let rows: Vec<(&str, Option<String>)> = vec![
        (
            "Abc-Domain",
            Some(
                "Member must satisfy regular expression pattern: [a-z][a-z0-9\\-]+".to_string(),
            ),
        ),
        (
            "1domain",
            Some(
                "Member must satisfy regular expression pattern: [a-z][a-z0-9\\-]+".to_string(),
            ),
        ),
        (
            "do_main",
            Some(
                "Member must satisfy regular expression pattern: [a-z][a-z0-9\\-]+".to_string(),
            ),
        ),
        (
            "ab",
            Some("Member must have length greater than or equal to 3".to_string()),
        ),
        (
            twenty_nine.as_str(),
            Some("Member must have length less than or equal to 28".to_string()),
        ),
        // Both published bounds, which the model admits: the route then answers about the
        // DOMAIN rather than about the request.
        ("abc", None),
        (twenty_eight.as_str(), None),
    ];

    for (sent, clause) in rows {
        let answer = describe_replication(&emulator, sent).await;
        match clause {
            Some(clause) => {
                let expected =
                    format!("1 validation error detected: Value '{sent}' at 'domainName' failed to satisfy constraint: {clause}");
                assert_eq!(
                    (answer.status, error_code(&answer).as_str()),
                    (400, VALIDATION),
                    "case DescribeDomainReplication({sent:?}): expected 400 {VALIDATION}, got {} \
                     with {:?}",
                    answer.status,
                    answer.body
                );
                assert_eq!(
                    answer.body["message"].as_str(),
                    Some(expected.as_str()),
                    "case DescribeDomainReplication({sent:?}): expected {expected:?}, got {:?}",
                    answer.body
                );
            }
            None => {
                assert_eq!(
                    (answer.status, error_code(&answer).as_str()),
                    (RECORDED_NOT_FOUND_STATUS, NOT_FOUND),
                    "case DescribeDomainReplication({sent:?}): a name the model admits is \
                     answered about the domain, got {} with {:?}",
                    answer.status,
                    answer.body
                );
                assert_eq!(
                    answer.body["message"].as_str(),
                    Some(not_found_message(&arn_like(&held, sent)).as_str()),
                    "case DescribeDomainReplication({sent:?}): got {:?}",
                    answer.body
                );
            }
        }
    }
}

/// The message an unsupported category earns: the value as sent, and the whole catalogue.
fn unsupported_category_message(sent: &str) -> String {
    format!(
        "Unsupported category: {sent}. Supported: [{}]",
        SUPPORTED_CATEGORIES.join(", ")
    )
}

/// RULE: `CompareDomainConfiguration` takes every category of the supported catalogue and
/// refuses any value outside it, naming the value as sent and the catalogue to choose from —
/// and that check runs BEFORE either domain is resolved.
#[tokio::test]
async fn compare_takes_every_supported_category_and_refuses_any_other() {
    let emulator = Emulator::new();
    let name = "compared-domain";
    let arn = given_domain(&emulator, name).await;
    let absent = arn_like(&arn, "never-created-domain");

    // Every category the catalogue publishes is taken — including the ones no case of this
    // suite configures — and a domain compared with itself diverges on none of them.
    for category in SUPPORTED_CATEGORIES {
        let answer = compare(
            &emulator,
            json!({
                "SourceDomainArn": arn,
                "TargetDomainArn": arn,
                "Categories": [category],
            }),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case CompareDomainConfiguration({category}): expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            (
                answer.body["Categories"][category]["Status"].as_str(),
                answer.body["Categories"][category]["Diverged"].as_i64(),
                answer.body["Categories"][category]["SourceOnly"].as_i64(),
                answer.body["Categories"][category]["TargetOnly"].as_i64(),
            ),
            (Some("compared"), Some(0), Some(0), Some(0)),
            "case CompareDomainConfiguration({category}) of a domain against itself: got {:?}",
            answer.body
        );
    }

    // A request naming NO category compares the whole catalogue.
    let every = compare(
        &emulator,
        json!({ "SourceDomainArn": arn, "TargetDomainArn": arn }),
    )
    .await;
    for category in SUPPORTED_CATEGORIES {
        assert_eq!(
            every.body["Categories"][category]["Status"].as_str(),
            Some("compared"),
            "case CompareDomainConfiguration(no Categories): expected {category} compared, got \
             {:?}",
            every.body
        );
    }

    // (case, the Categories value, the value the refusal names)
    let rows: Vec<(&str, Value, String)> = vec![
        (
            "a modelled option container's name",
            json!(["ClusterConfig"]),
            "ClusterConfig".to_string(),
        ),
        (
            "a supported category spelled with hyphens",
            json!(["cluster-settings"]),
            "cluster-settings".to_string(),
        ),
        (
            "a supported category in upper case",
            json!(["CLUSTER_SETTINGS"]),
            "CLUSTER_SETTINGS".to_string(),
        ),
        (
            "a near miss of a supported category",
            json!(["aliasess"]),
            "aliasess".to_string(),
        ),
        (
            "an unsupported category after a supported one",
            json!(["templates", "not_a_real_category"]),
            "not_a_real_category".to_string(),
        ),
        (
            "a category that is not even text",
            json!([7]),
            "7".to_string(),
        ),
    ];
    for (case, categories, named) in rows {
        // Against a source that does not resolve either: the category check comes first, so
        // the category is what the request is refused for.
        let answer = compare(
            &emulator,
            json!({
                "SourceDomainArn": absent,
                "TargetDomainArn": arn,
                "Categories": categories,
            }),
        )
        .await;
        assert_eq!(
            (answer.status, error_code(&answer).as_str()),
            (400, VALIDATION),
            "case {case}: expected 400 {VALIDATION}, got {} with {:?}",
            answer.status,
            answer.body
        );
        assert_eq!(
            answer.body["message"].as_str(),
            Some(unsupported_category_message(&named).as_str()),
            "case {case}: expected the unsupported-category refusal naming {named}, got {:?}",
            answer.body
        );
    }
}

/// RULE: a side of the comparison must BE a domain ARN, and the format check runs before
/// either side is resolved; a well-formed ARN naming a domain this account does not hold is
/// then refused naming which side it was.
#[tokio::test]
async fn compare_refuses_a_side_that_is_not_a_domain_arn_before_resolving_either() {
    let emulator = Emulator::new();
    let arn = given_domain(&emulator, "resolvable-side").await;

    let malformed = [
        "not-an-arn",
        "arn:aws:es:us-east-1:123456789012:domain",
        "arn:bws:es:us-east-1:123456789012:domain/some-domain",
        "arn:aws:es:us-east-1:123456789012:application/some-app",
        "arn:aws:es:us-east-1:123456789012:domain/",
    ];
    for sent in malformed {
        for (side, body) in [
            (
                "SourceDomainArn",
                json!({ "SourceDomainArn": sent, "TargetDomainArn": arn }),
            ),
            (
                "TargetDomainArn",
                json!({ "SourceDomainArn": arn, "TargetDomainArn": sent }),
            ),
        ] {
            let case = format!("CompareDomainConfiguration({side} = {sent:?})");
            let answer = compare(&emulator, body).await;
            assert_eq!(
                (answer.status, error_code(&answer).as_str()),
                (400, VALIDATION),
                "case {case}: expected 400 {VALIDATION}, got {} with {:?}",
                answer.status,
                answer.body
            );
            let expected = format!(
                "Invalid domain ARN format. Expected domain ARN (arn:aws*:*:*:*:domain/*) but \
                 received: {sent}. "
            );
            assert_eq!(
                answer.body["message"].as_str(),
                Some(expected.as_str()),
                "case {case}: expected {expected:?}, got {:?}",
                answer.body
            );
        }
    }

    // A well-formed ARN naming a domain nobody created: the refusal names the SIDE and the
    // domain its ARN named, and the source is resolved first.
    for (side, named, body) in [
        (
            "Source",
            "absent-source",
            json!({
                "SourceDomainArn": arn_like(&arn, "absent-source"),
                "TargetDomainArn": arn_like(&arn, "absent-target"),
            }),
        ),
        (
            "Target",
            "absent-target",
            json!({
                "SourceDomainArn": arn,
                "TargetDomainArn": arn_like(&arn, "absent-target"),
            }),
        ),
    ] {
        let case = format!("CompareDomainConfiguration({side} names no domain)");
        let answer = compare(&emulator, body).await;
        assert_eq!(
            (answer.status, error_code(&answer).as_str()),
            (400, VALIDATION),
            "case {case}: expected 400 {VALIDATION}, got {} with {:?}",
            answer.status,
            answer.body
        );
        let expected = format!("{side} domain {named}: Domain {named} not found.");
        assert_eq!(
            answer.body["message"].as_str(),
            Some(expected.as_str()),
            "case {case}: expected {expected:?}, got {:?}",
            answer.body
        );
    }
}

/// The counts one category's comparison is expected to report.
struct CategoryCounts {
    category: &'static str,
    diverged: i64,
    source_only: i64,
    target_only: i64,
}

impl CategoryCounts {
    /// `(category, Diverged, SourceOnly, TargetOnly)`, as a row writes it.
    fn of(category: &'static str, diverged: i64, source_only: i64, target_only: i64) -> Self {
        Self {
            category,
            diverged,
            source_only,
            target_only,
        }
    }
}

/// One comparison to drive: which domains are compared, and what each category it names is
/// expected to count.
struct DiffRow<'a> {
    case: &'static str,
    source: &'a str,
    target: &'a str,
    expected: Vec<CategoryCounts>,
}

/// RULE: a comparison counts the configuration the two domains HOLD, per category: an entry
/// only one side holds counts on that side, and an entry both hold with different content
/// counts as diverged.
///
/// The configuration is written through each domain's OWN data plane — the surface that holds
/// it — so what is compared is state a client wrote, not anything this suite put in the
/// store.
#[tokio::test]
async fn compare_counts_the_configuration_each_domain_holds() {
    let emulator = Emulator::new();
    let source = "diffed-source";
    let target = "diffed-target";
    let source_arn = given_domain(&emulator, source).await;
    let target_arn = given_domain(&emulator, target).await;

    // One setting both clusters hold at DIFFERENT values, and one pipeline only the source
    // holds.
    for (domain, value) in [(source, "true"), (target, "false")] {
        let written = rest_prefixed(
            &emulator,
            domain,
            "PUT",
            "/_cluster/settings",
            Some(json!({ "persistent": { "action.auto_create_index": value } })),
        )
        .await;
        assert_eq!(
            written.status, 200,
            "case PUT /_cluster/settings on {domain}: expected 200, got {} with {:?}",
            written.status, written.body
        );
    }
    let pipeline = rest_prefixed(
        &emulator,
        source,
        "PUT",
        "/_ingest/pipeline/only-on-the-source",
        Some(json!({
            "description": "a pipeline only the source holds",
            "processors": [{ "set": { "field": "marked", "value": "yes" } }],
        })),
    )
    .await;
    assert_eq!(
        pipeline.status, 200,
        "case PUT /_ingest/pipeline on {source}: expected 200, got {} with {:?}",
        pipeline.status, pipeline.body
    );

    // The comparisons to drive, and the counts each expects per category.
    let rows: Vec<DiffRow<'_>> = vec![
        DiffRow {
            case: "the source against the target",
            source: &source_arn,
            target: &target_arn,
            expected: vec![
                CategoryCounts::of("cluster_settings", 1, 0, 0),
                CategoryCounts::of("ingest_pipelines", 0, 1, 0),
                CategoryCounts::of("templates", 0, 0, 0),
            ],
        },
        DiffRow {
            case: "the same two sides the other way round",
            source: &target_arn,
            target: &source_arn,
            expected: vec![
                CategoryCounts::of("cluster_settings", 1, 0, 0),
                CategoryCounts::of("ingest_pipelines", 0, 0, 1),
                CategoryCounts::of("templates", 0, 0, 0),
            ],
        },
        DiffRow {
            case: "the source against itself",
            source: &source_arn,
            target: &source_arn,
            expected: vec![
                CategoryCounts::of("cluster_settings", 0, 0, 0),
                CategoryCounts::of("ingest_pipelines", 0, 0, 0),
                CategoryCounts::of("templates", 0, 0, 0),
            ],
        },
    ];

    for row in rows {
        let categories: Vec<&str> = row
            .expected
            .iter()
            .map(|counts| counts.category)
            .collect();
        let case = row.case;
        let answer = compare(
            &emulator,
            json!({
                "SourceDomainArn": row.source,
                "TargetDomainArn": row.target,
                "Categories": categories,
            }),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        for counts in row.expected {
            let CategoryCounts {
                category,
                diverged,
                source_only,
                target_only,
            } = counts;
            let reported = &answer.body["Categories"][category];
            assert_eq!(
                (
                    reported["Status"].as_str(),
                    reported["Diverged"].as_i64(),
                    reported["SourceOnly"].as_i64(),
                    reported["TargetOnly"].as_i64(),
                ),
                (
                    Some("compared"),
                    Some(diverged),
                    Some(source_only),
                    Some(target_only)
                ),
                "case {case} ({category}): expected diverged {diverged}, source-only \
                 {source_only}, target-only {target_only}, got {reported:?}"
            );
        }
    }
}
