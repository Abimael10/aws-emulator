// SPDX-License-Identifier: CC0-1.0

//! The SSH key pair a host names is a key pair the ACCOUNT holds.
//!
//! An Amazon EVS host IS an EC2 bare metal instance ("An ESX host that runs on an Amazon
//! EC2 bare metal instance", `Host` doc), and `keyName` is "the name of the SSH key that
//! is used to access the host" (`HostInfoForCreate$keyName`) — the "EC2 SSH keypair name"
//! a `hosts` entry supplies (`CreateEnvironment$hosts` doc). A key pair is an Amazon EC2
//! resource of the caller's own account, minted there by `CreateKeyPair` or
//! `ImportKeyPair`, and Amazon EVS creates none, so the rules under test are:
//!
//! * a `keyName` the account's own Amazon EC2 key pair store holds is accepted, reported
//!   back on every later read of the host, and carried onto the EC2 instance the host
//!   runs on; and
//! * a `keyName` that store holds no key pair for is refused with the operation's
//!   `ValidationException`, whose `fieldList` names the member that named it, and nothing
//!   is stored and no instance launched — the rule the sibling suite
//!   `host_placement_references` asserts for `placementGroupId` and `dedicatedHostId`,
//!   which are EC2 resources of that same account.
//!
//! Both services are mounted on ONE shared engine, and every call goes over the wire a
//! client makes it on — awsJson1_0 for Amazon EVS, ec2Query for Amazon EC2 — so every key
//! pair an accepted row names is minted by Amazon EC2 itself, exactly as a caller would
//! mint it, and the names the refused rows carry are names EC2's own `DescribeKeyPairs`
//! refuses too.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use service_com_amazonaws_evs_amazonelasticvmwareservice as evs;
use smithy_server_runtime::MultiServiceRouter;

use crate::create_environment_validation::{assert_aggregated_refusal, environment_count};
use crate::environment_lifecycle::{
    call, error_type, register_network, vlans_with_prefix, REGION,
};
use crate::vlan_subnet_propagation::{call_ec2, element, elements};

/// Long enough that every lifecycle transition this service owes is due when the store is
/// settled against it (each span is under a second by construction). No test here waits in
/// wall-clock time: [`Emulator::settle`] applies the transitions a call made after this
/// span would.
const PAST_THE_TRANSITION: Duration = Duration::from_millis(900);

/// The VLAN subnet prefix length every environment here is created with: a /24 leaves
/// room for every host a table adds.
const VLAN_PREFIX_LEN: u8 = 24;

/// The error code Amazon EC2 answers for a key pair name the account holds none of
/// (`InvalidKeyPair.NotFound`, EC2's own error-code list), used to show that a name Amazon
/// EVS refuses is a name the OTHER service refuses as well.
const INVALID_KEY_PAIR_NOT_FOUND: &str = "InvalidKeyPair.NotFound";

/// The longest `keyName` the model admits: "len 1..255"
/// (`smithy_query(query="shape", shape="HostInfoForCreate")` on `keyName`, whose pattern
/// is `^[a-zA-Z0-9_-]+$`), so the table spans both published bounds of the member.
const MAX_KEY_NAME_CHARS: usize = 255;

/// What a row expects of the create it drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The request is served: HTTP 200, and the host is stored with the key it named.
    Accepted,
    /// The request is refused with a `ValidationException` naming `host.keyName`.
    Refused,
}

/// One emulator serving BOTH services off one shared engine.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These tests exercise behaviour, not signing.
        router.set_request_authentication(false);
        // An environment is created only for an account holding a Premium Support
        // subscription (`tests/all/premium_support_entitlement.rs` owns that rule).
        crate::premium_support_entitlement::grant_premium_support(&engine);
        router.register_service(
            service_com_amazonaws_evs_amazonelasticvmwareservice::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(ec2::EmulatedService::new(Arc::clone(&engine)));
        // No key pair is registered here: every pair this suite's account holds is minted
        // through Amazon EC2 itself ([`Emulator::key_pair`]).
        Self { router, engine }
    }

    /// A VPC and a subnet inside it in the account's own EC2 store, as
    /// `CreateEnvironment` requires — a fresh pair per environment, because Amazon EVS
    /// allows one deployed environment per VPC.
    fn fresh_network(&self) -> (String, String) {
        let ec2_state = ec2::state::service_state(&*self.engine)
            .expect("the in-memory engine carries the account's EC2 store");
        let (vpc_id, subnet_id) = {
            let mut store = ec2_state.lock();
            (store.next_id("vpc"), store.next_id("subnet"))
        };
        register_network(&self.engine, &vpc_id, &subnet_id);
        (vpc_id, subnet_id)
    }

    /// A `CreateEnvironment` request that satisfies every other rule, carrying `hosts`
    /// when given.
    fn create_request(&self, environment_name: &str, hosts: Option<&Value>) -> Value {
        let (vpc_id, subnet_id) = self.fresh_network();
        let mut request = json!({
            "vpcId": vpc_id,
            "serviceAccessSubnetId": subnet_id,
            "vcfVersion": "VCF-5.2.1",
            "termsAccepted": true,
            "environmentName": environment_name,
            "initialVlans": vlans_with_prefix(VLAN_PREFIX_LEN),
        });
        if let Some(hosts) = hosts {
            request["hosts"] = hosts.clone();
        }
        request
    }

    /// Settle every lifecycle transition this service owes, WITHOUT sleeping for it: a
    /// new environment is `CREATING` until the move the clock owes it comes due, and both
    /// adding a host to it and listing its hosts need that move to have happened. The
    /// stored due instants are compared against the instant handed in, so this applies
    /// exactly the transitions a call made [`PAST_THE_TRANSITION`] from now would.
    fn settle(&self) {
        let state = evs::state::service_state(&*self.engine)
            .expect("the in-memory engine carries this service's store");
        let past_the_transition = std::time::Instant::now() + PAST_THE_TRANSITION;
        let mut store = state.lock();
        for environment in store.environments.values_mut() {
            environment.materialise_due_transition(past_the_transition);
        }
        store.recompute_environment_checks();
    }

    /// Create an environment and settle the deployment the clock owes it, so a host can
    /// be added to it.
    async fn deployed_environment(&self, case: &str) -> String {
        let request = self.create_request(case, None);
        let (status, body) = call(&self.router, "CreateEnvironment", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateEnvironment expected 200, got {status} ({}) {body}",
            error_type(&body)
        );
        let environment = body["environment"]["environmentId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no environmentId in {body}"))
            .to_string();
        self.settle();
        environment
    }

    /// A key pair the account really holds, minted by Amazon EC2's own `CreateKeyPair`.
    ///
    /// `key_type` is a member of the modelled `KeyType` catalog, or `None` for the type a
    /// create defaults to.
    async fn key_pair(&self, key_name: &str, key_type: Option<&str>) {
        let mut params: Vec<(&str, &str)> = vec![("KeyName", key_name)];
        if let Some(key_type) = key_type {
            params.push(("KeyType", key_type));
        }
        let (status, body) = call_ec2(&self.router, REGION, "CreateKeyPair", &params).await;
        assert_eq!(
            status, 200,
            "CreateKeyPair {key_name:?}: expected 200, got {status} {body}"
        );
    }

    /// Remove a key pair from the account's store through Amazon EC2's own
    /// `DeleteKeyPair`, so the name names nothing again.
    async fn delete_key_pair(&self, key_name: &str) {
        let (status, body) =
            call_ec2(&self.router, REGION, "DeleteKeyPair", &[("KeyName", key_name)]).await;
        assert_eq!(
            status, 200,
            "DeleteKeyPair {key_name:?}: expected 200, got {status} {body}"
        );
    }

    /// Whether Amazon EC2 itself reports a key pair of this name for the account: the
    /// other side of the same fact Amazon EVS resolves a `keyName` against.
    async fn ec2_holds_key_pair(&self, key_name: &str) -> bool {
        let (status, body) = call_ec2(
            &self.router,
            REGION,
            "DescribeKeyPairs",
            &[("KeyName.1", key_name)],
        )
        .await;
        if status == 200 {
            return elements(&body, "keyName").iter().any(|held| held == key_name);
        }
        assert_eq!(
            element(&body, "Code").as_deref(),
            Some(INVALID_KEY_PAIR_NOT_FOUND),
            "DescribeKeyPairs {key_name:?}: expected {INVALID_KEY_PAIR_NOT_FOUND}, got \
             {status} {body}"
        );
        false
    }

    /// Add one host naming `key_name`, and answer what `CreateEnvironmentHost` answered.
    async fn create_host(
        &self,
        environment: &str,
        host_name: &str,
        key_name: &str,
    ) -> (u16, Value) {
        let request = json!({
            "environmentId": environment,
            "host": {
                "hostName": host_name,
                "keyName": key_name,
                "instanceType": "i4i.metal",
            },
        });
        call(&self.router, "CreateEnvironmentHost", &request).await
    }

    /// Every host of the environment, as `ListEnvironmentHosts` reports them.
    async fn hosts(&self, environment: &str) -> Vec<Value> {
        let (status, body) = call(
            &self.router,
            "ListEnvironmentHosts",
            &json!({ "environmentId": environment }),
        )
        .await;
        assert_eq!(status, 200, "ListEnvironmentHosts: got {status} {body}");
        body["environmentHosts"]
            .as_array()
            .unwrap_or_else(|| panic!("ListEnvironmentHosts: no environmentHosts in {body}"))
            .clone()
    }

    /// Every instance id the account's own EC2 store reports, so a refused request can be
    /// shown to have launched none.
    async fn instance_ids(&self) -> Vec<String> {
        let (status, body) = call_ec2(&self.router, REGION, "DescribeInstances", &[]).await;
        assert_eq!(
            status, 200,
            "DescribeInstances: expected 200, got {status} {body}"
        );
        let mut ids = elements(&body, "instanceId");
        ids.sort();
        ids
    }

    /// The `keyName` Amazon EC2 reports for one instance of the account.
    async fn instance_key_name(&self, instance_id: &str) -> Option<String> {
        let (status, body) = call_ec2(
            &self.router,
            REGION,
            "DescribeInstances",
            &[("InstanceId.1", instance_id)],
        )
        .await;
        assert_eq!(
            status, 200,
            "DescribeInstances {instance_id:?}: expected 200, got {status} {body}"
        );
        element(&body, "keyName")
    }
}

/// The `fieldList` member names a `ValidationException` body carries.
fn refused_members(body: &Value) -> Vec<String> {
    body["fieldList"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// RULE: a host is reached with an SSH key pair the ACCOUNT's own Amazon EC2 store holds,
/// so a `keyName` that store holds no key pair for is refused and nothing is created.
///
/// One row per form the name comes in — a pair of each key TYPE the account minted, the
/// shortest and longest names the member admits, a name nothing ever held, a held name
/// spelled in another case (a key pair name is matched byte for byte, as Amazon EC2
/// matches one), and a name whose key pair the account DELETED — so the one name an issue
/// cited is one row among many.
#[tokio::test]
async fn a_host_names_an_ssh_key_pair_the_account_holds() {
    let emulator = Emulator::new();
    let environment = emulator.deployed_environment("host-key-pairs").await;

    // The key pairs the account really holds, minted by Amazon EC2 itself: one of each
    // type the modelled `KeyType` catalog offers, one at each published length bound of
    // `keyName`, one spelled in mixed case, and one that is removed again below.
    let longest_held = "k".repeat(MAX_KEY_NAME_CHARS);
    emulator.key_pair("evs-key", Some("ed25519")).await;
    // No `KeyType`, so the pair takes the type a create defaults to ("Default: rsa",
    // `CreateKeyPairRequest$KeyType`).
    emulator.key_pair("rsa-default-key", None).await;
    emulator.key_pair("a", Some("ed25519")).await;
    emulator.key_pair(&longest_held, Some("ed25519")).await;
    emulator.key_pair("Mixed_Case-Key", Some("ed25519")).await;
    emulator.key_pair("gone-key", Some("ed25519")).await;
    emulator.delete_key_pair("gone-key").await;

    let rows: Vec<(&str, String, Expected)> = vec![
        (
            "an ed25519 key pair the account minted",
            "evs-key".to_string(),
            Expected::Accepted,
        ),
        (
            "a key pair of the type a create defaults to",
            "rsa-default-key".to_string(),
            Expected::Accepted,
        ),
        (
            "the shortest name the member admits, held",
            "a".to_string(),
            Expected::Accepted,
        ),
        (
            "the longest name the member admits, held",
            longest_held.clone(),
            Expected::Accepted,
        ),
        (
            "a held name in its own spelling",
            "Mixed_Case-Key".to_string(),
            Expected::Accepted,
        ),
        (
            "a name the account holds no key pair for",
            "no-such-key".to_string(),
            Expected::Refused,
        ),
        (
            // A key pair name is matched byte for byte, so another spelling of a held
            // name names no key pair of the account.
            "a held name spelled in another case",
            "mixed_case-key".to_string(),
            Expected::Refused,
        ),
        (
            "a name whose key pair the account deleted",
            "gone-key".to_string(),
            Expected::Refused,
        ),
        (
            "the longest name the member admits, held by nothing",
            "z".repeat(MAX_KEY_NAME_CHARS),
            Expected::Refused,
        ),
    ];

    for (index, (case, key_name, expected)) in rows.iter().enumerate() {
        let host_name = format!("esx-{index}");
        let instances_before = emulator.instance_ids().await;
        let hosts_before = emulator.hosts(&environment).await.len();
        let (status, body) = emulator
            .create_host(&environment, &host_name, key_name)
            .await;
        // Whether the ACCOUNT holds the pair is Amazon EC2's own fact, and it is the fact
        // the row expects Amazon EVS to have resolved.
        let held = emulator.ec2_holds_key_pair(key_name).await;
        assert_eq!(
            held,
            *expected == Expected::Accepted,
            "case {case:?}: Amazon EC2 holding the key pair must be what the row expects, \
             got held={held}"
        );

        match expected {
            Expected::Accepted => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200, got {status} ({}) {body}",
                    error_type(&body)
                );
                assert_eq!(
                    body["host"]["keyName"],
                    json!(key_name),
                    "case {case:?}: the created host must report the key it names, got {}",
                    body["host"]["keyName"]
                );
                // The key the host was admitted with is reported by every later read of
                // it, and carried onto the EC2 instance the host runs on.
                let listed = emulator.hosts(&environment).await;
                assert_eq!(
                    listed.len(),
                    hosts_before + 1,
                    "case {case:?}: expected one more stored host, got {listed:?}"
                );
                let stored = listed
                    .iter()
                    .find(|stored| stored["hostName"] == json!(host_name))
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: {host_name} is not reported in {listed:?}")
                    });
                assert_eq!(
                    stored["keyName"],
                    json!(key_name),
                    "case {case:?}: ListEnvironmentHosts keyName: expected {key_name:?}, \
                     got {}",
                    stored["keyName"]
                );
                let instance_id = stored["ec2InstanceId"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: the host reports no ec2InstanceId in {stored}")
                    })
                    .to_string();
                let reported = emulator.instance_key_name(&instance_id).await;
                assert_eq!(
                    reported.as_deref(),
                    Some(key_name.as_str()),
                    "case {case:?}: the instance the host runs on must carry the key pair \
                     the request named, got {reported:?}"
                );
            }
            Expected::Refused => {
                assert_aggregated_refusal(case, status, &body, &["host.keyName"]);
                assert_eq!(
                    body["host"],
                    Value::Null,
                    "case {case:?}: a refused request must report no host, got {}",
                    body["host"]
                );
                // The refusal names the value the request gave, so a caller can see which
                // key pair it has to create.
                let clause = body["fieldList"][0]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    clause.contains(key_name.as_str()),
                    "case {case:?}: the clause must name the key pair the request gave, \
                     got {clause:?}"
                );
                // And a refused request stores no host and launches no instance.
                let listed = emulator.hosts(&environment).await;
                assert_eq!(
                    listed.len(),
                    hosts_before,
                    "case {case:?}: a refused request must store no host, got {listed:?}"
                );
                assert_eq!(
                    emulator.instance_ids().await,
                    instances_before,
                    "case {case:?}: a refused request must launch no instance"
                );
                assert!(
                    !refused_members(&body).iter().any(|name| name == "host"),
                    "case {case:?}: only the member that named the key pair is refused, \
                     got {:?}",
                    refused_members(&body)
                );
            }
        }
    }
}

/// RULE: the same key pair every `hosts` entry of a `CreateEnvironment` names is resolved
/// against that same store, and the refusal names the ENTRY that named one.
///
/// `hosts` takes four `HostInfoForCreate` entries, so the rule is asserted at the path a
/// caller wrote: two entries name key pairs the account holds and two name names it holds
/// none for, in positions an index-blind reading would confuse.
#[tokio::test]
async fn an_environment_request_resolves_the_key_pairs_of_its_own_hosts() {
    let emulator = Emulator::new();
    emulator.key_pair("evs-key", Some("ed25519")).await;
    emulator.key_pair("second_key-2", Some("ed25519")).await;

    let hosts = json!([
        { "hostName": "esx-0", "keyName": "evs-key", "instanceType": "i4i.metal" },
        { "hostName": "esx-1", "keyName": "no-such-key", "instanceType": "i4i.metal" },
        { "hostName": "esx-2", "keyName": "second_key-2", "instanceType": "i4i.metal" },
        { "hostName": "esx-3", "keyName": "EVS-KEY", "instanceType": "i4i.metal" },
    ]);
    let before = environment_count(&emulator.router).await;
    let request = emulator.create_request("hosts-key-pairs", Some(&hosts));
    let (status, body) = call(&emulator.router, "CreateEnvironment", &request).await;
    assert_aggregated_refusal(
        "two entries name key pairs the account does not hold",
        status,
        &body,
        &["hosts[1].keyName", "hosts[3].keyName"],
    );
    let after = environment_count(&emulator.router).await;
    assert_eq!(
        after, before,
        "a refused create must create nothing, got {after} environment(s) where there \
         were {before}"
    );

    // The same request with every entry naming a key pair the account holds is served,
    // and each host reports the key its entry named.
    let hosts = json!([
        { "hostName": "esx-0", "keyName": "evs-key", "instanceType": "i4i.metal" },
        { "hostName": "esx-1", "keyName": "second_key-2", "instanceType": "i4i.metal" },
        { "hostName": "esx-2", "keyName": "evs-key", "instanceType": "i4i.metal" },
        { "hostName": "esx-3", "keyName": "second_key-2", "instanceType": "i4i.metal" },
    ]);
    let request = emulator.create_request("hosts-key-pairs-held", Some(&hosts));
    let (status, body) = call(&emulator.router, "CreateEnvironment", &request).await;
    assert_eq!(
        status, 200,
        "every entry naming a held key pair: expected HTTP 200, got {status} ({}) {body}",
        error_type(&body)
    );
    let environment = body["environment"]["environmentId"]
        .as_str()
        .unwrap_or_else(|| panic!("no environmentId in {body}"))
        .to_string();
    // A list addressed to an environment that is still initializing is refused, so the
    // move the clock owes the new environment is settled first.
    emulator.settle();
    let listed = emulator.hosts(&environment).await;
    let reported: Vec<(String, String)> = listed
        .iter()
        .map(|host| {
            (
                host["hostName"].as_str().unwrap_or_default().to_string(),
                host["keyName"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let expected: Vec<(String, String)> = vec![
        ("esx-0".to_string(), "evs-key".to_string()),
        ("esx-1".to_string(), "second_key-2".to_string()),
        ("esx-2".to_string(), "evs-key".to_string()),
        ("esx-3".to_string(), "second_key-2".to_string()),
    ];
    assert_eq!(
        reported, expected,
        "every added host must report the key pair its entry named, got {reported:?}"
    );
}
