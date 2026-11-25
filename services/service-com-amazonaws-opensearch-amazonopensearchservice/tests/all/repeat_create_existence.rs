// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what the control plane answers about a domain the account ALREADY
//! holds: the `CreateDomain` that names it again, and the two read paths that report it.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The harness comes from [`super::domain_lifecycle`] and
//! the batch read from [`super::domain_batch_describe`]; this suite adds no second copy of
//! either.
//!
//! The rules under test are not one recorded call:
//!
//! * a `CreateDomain` naming a domain the account already holds in that Region creates no
//!   second domain and moves nothing about the one that is there — it answers that domain's
//!   OWN current status, whatever configuration the repeat asked for — so the table below
//!   spans the kinds of thing a repeat can ask for differently (a count, an instance type,
//!   an enum, an engine release, a nested number, a whole container left out, a policy
//!   document, a map entry, a snapshot hour, a tag set, and every one of them at once),
//!   with the identical request as one row rather than the whole case;
//! * a name the account holds NO domain under is a free name, so a create after the delete
//!   that freed it provisions a FRESH domain with the configuration it asked for;
//! * and both read paths resolve a name against the same stored records, so what
//!   `DescribeDomain` reports for a name is exactly what `DescribeDomains` reports for it in
//!   a batch.

use serde_json::{json, Value};

use super::domain_batch_describe::describe_domains;
use super::domain_lifecycle::{
    baseline_create, settled_domain, Emulator, PREFIX, RECORDED_NOT_FOUND_STATUS,
};

/// One repeat `CreateDomain` to drive: how the row changes the request its domain was
/// created with.
struct Repeat {
    case: &'static str,
    change: fn(&mut Value),
}

/// The repeats driven below. Each row asks for something the stored domain does NOT hold,
/// one kind of value at a time, so the rule is asserted over the shapes it generalises
/// over rather than over one recorded body.
const REPEATS: &[Repeat] = &[
    Repeat {
        case: "the identical request",
        change: |_| {},
    },
    Repeat {
        case: "another data node count",
        change: |body| body["ClusterConfig"]["InstanceCount"] = json!(4),
    },
    Repeat {
        case: "another instance type",
        change: |body| body["ClusterConfig"]["InstanceType"] = json!("m5.large.search"),
    },
    Repeat {
        case: "another address type",
        change: |body| body["IPAddressType"] = json!("dualstack"),
    },
    Repeat {
        case: "another engine release",
        change: |body| body["EngineVersion"] = json!("OpenSearch_2.13"),
    },
    Repeat {
        case: "a larger volume",
        change: |body| body["EBSOptions"]["VolumeSize"] = json!(60),
    },
    Repeat {
        case: "no storage container at all",
        change: |body| {
            if let Some(members) = body.as_object_mut() {
                members.remove("EBSOptions");
            }
        },
    },
    Repeat {
        case: "another access policy document",
        change: |body| {
            body["AccessPolicies"] = json!("{\"Version\":\"2012-10-17\",\"Statement\":[]}");
        },
    },
    Repeat {
        case: "another advanced option entry",
        change: |body| body["AdvancedOptions"]["indices.fielddata.cache.size"] = json!("40"),
    },
    Repeat {
        case: "another automated snapshot hour",
        change: |body| body["SnapshotOptions"]["AutomatedSnapshotStartHour"] = json!(7),
    },
    Repeat {
        case: "a tag set the domain does not carry",
        change: |body| body["TagList"] = json!([{ "Key": "team", "Value": "search" }]),
    },
    Repeat {
        case: "every one of them at once",
        change: |body| {
            body["ClusterConfig"] = json!({ "InstanceType": "m5.large.search", "InstanceCount": 4 });
            body["IPAddressType"] = json!("dualstack");
            body["EngineVersion"] = json!("OpenSearch_2.13");
            body["EBSOptions"] = json!({ "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 60 });
            body["AccessPolicies"] = json!("{\"Version\":\"2012-10-17\",\"Statement\":[]}");
            body["AdvancedOptions"] = json!({ "indices.fielddata.cache.size": "40" });
            body["SnapshotOptions"] = json!({ "AutomatedSnapshotStartHour": 7 });
            body["TagList"] = json!([{ "Key": "team", "Value": "search" }]);
        },
    },
];

/// The name the row at `index` drives, inside `DomainName`'s own length bound.
fn row_name(index: usize) -> String {
    format!("held-domain-{index}")
}

/// The `DomainStatus` of an accepted answer, with the case named on failure.
fn domain_status(body: &Value, case: &str) -> Value {
    let status = body["DomainStatus"].clone();
    assert!(
        status.is_object(),
        "case {case:?}: expected a DomainStatus object, got {body:?}"
    );
    status
}

/// The one entry `DescribeDomains` reports for `name`, with the case named on failure.
async fn batch_entry(emulator: &Emulator, name: &str, case: &str) -> Value {
    let answer = describe_domains(emulator, &[name]).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected DescribeDomains([{name:?}]) to answer 200, got {} with {:?}",
        answer.status, answer.body
    );
    let entries = answer.body["DomainStatusList"]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a DomainStatusList array, got {:?}",
                answer.body
            )
        })
        .clone();
    assert_eq!(
        entries.len(),
        1,
        "case {case:?}: expected DescribeDomains([{name:?}]) to report the one domain the \
         account holds under that name, got {entries:?}"
    );
    entries[0].clone()
}

/// RULE: a domain name is unique within the account and Region, so a `CreateDomain` naming a
/// domain the account already holds mints no second record and moves nothing about the one
/// that is there — it answers that domain's own current status, and the stored configuration
/// stays the one the first create provisioned, whatever the repeat asked for.
#[tokio::test]
async fn a_create_of_a_held_name_answers_that_domain_and_changes_nothing() {
    let emulator = Emulator::new();

    // Every domain is created first, so the provisioning spans run together and the loop
    // below waits once for all of them rather than once per row.
    for (index, repeat) in REPEATS.iter().enumerate() {
        let name = row_name(index);
        let created = emulator.create(baseline_create(&name)).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: expected the first create accepted, got {} with {:?}",
            repeat.case, created.status, created.body
        );
    }

    for (index, repeat) in REPEATS.iter().enumerate() {
        let case = repeat.case;
        let name = row_name(index);
        // The repeat is driven against a domain whose provisioning has landed, so what it
        // answers is that domain's settled state — the state a read of it reports at the
        // same instant.
        let before = settled_domain(&emulator, &name).await;

        let mut body = baseline_create(&name);
        (repeat.change)(&mut body);
        let again = emulator.create(body).await;
        assert_eq!(
            again.status, 200,
            "case {case:?}: expected the create of a held name to answer 200, got {} with {:?}",
            again.status, again.body
        );
        assert_eq!(
            domain_status(&again.body, case),
            before,
            "case {case:?}: expected the status of the domain the account already holds, got {:?}",
            again.body["DomainStatus"]
        );

        // Nothing moved: the record a later caller reads is the one the first create left,
        // and both read paths report that same record for that name.
        let after = domain_status(&emulator.describe(&name).await.body, case);
        assert_eq!(
            after, before,
            "case {case:?}: expected the stored domain untouched by the repeat, got {after:?}"
        );
        assert_eq!(
            batch_entry(&emulator, &name, case).await,
            before,
            "case {case:?}: expected the batch read to report the same record as the single read"
        );
    }

    // ... and no row added a domain: the account holds exactly the one per row it created.
    let listed = emulator
        .call("GET", &format!("{PREFIX}/domain"), None)
        .await;
    let names: Vec<&str> = listed.body["DomainNames"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["DomainName"].as_str())
                .collect()
        })
        .unwrap_or_default();
    let expected: Vec<String> = (0..REPEATS.len()).map(row_name).collect();
    assert_eq!(
        names.len(),
        expected.len(),
        "case the whole table: expected exactly {} domains, got {names:?}",
        expected.len()
    );
    for name in &expected {
        assert!(
            names.iter().any(|listed| listed == name),
            "case the whole table: expected {name:?} among the domains the account holds, got \
             {names:?}"
        );
    }
}

/// RULE: a name the account holds no domain under is free, so a create naming one provisions
/// a FRESH domain — the resolution above is about the domain that is there, not about the
/// name having been used once.
#[tokio::test]
async fn a_name_freed_by_a_delete_takes_a_fresh_domain() {
    let emulator = Emulator::new();
    let name = "freed-domain";

    let created = emulator.create(baseline_create(name)).await;
    assert_eq!(
        created.status, 200,
        "case the first create: expected 200, got {} with {:?}",
        created.status, created.body
    );
    let first = settled_domain(&emulator, name).await;

    let deleted = emulator.delete(name).await;
    assert_eq!(
        deleted.status, 200,
        "case DeleteDomain({name:?}): expected 200, got {} with {:?}",
        deleted.status, deleted.body
    );
    let gone = emulator.describe(name).await;
    assert_eq!(
        gone.status, RECORDED_NOT_FOUND_STATUS,
        "case DescribeDomain({name:?}, deleted): expected the missing-resource refusal, got {} \
         with {:?}",
        gone.status, gone.body
    );

    let mut body = baseline_create(name);
    body["ClusterConfig"]["InstanceCount"] = json!(4);
    let again = emulator.create(body).await;
    assert_eq!(
        again.status, 200,
        "case the create after the delete: expected 200, got {} with {:?}",
        again.status, again.body
    );
    let fresh = domain_status(&again.body, "the create after the delete");
    // A domain that is being provisioned, not the settled record the delete took away: the
    // create did the work again rather than resolving to something.
    assert_eq!(
        fresh["Processing"],
        json!(true),
        "case the create after the delete: expected a domain being provisioned, got {fresh:?}"
    );
    assert_eq!(
        first["Processing"],
        json!(false),
        "case the create after the delete: expected the deleted domain to have been settled, \
         got {first:?}"
    );
    assert_eq!(
        fresh["ClusterConfig"]["InstanceCount"],
        json!(4),
        "case the create after the delete: expected the configuration it asked for, got {:?}",
        fresh["ClusterConfig"]
    );
    let read = domain_status(
        &emulator.describe(name).await.body,
        "the create after the delete",
    );
    assert_eq!(
        read["ClusterConfig"]["InstanceCount"],
        json!(4),
        "case the create after the delete: expected the fresh configuration stored, got {:?}",
        read["ClusterConfig"]
    );
}
