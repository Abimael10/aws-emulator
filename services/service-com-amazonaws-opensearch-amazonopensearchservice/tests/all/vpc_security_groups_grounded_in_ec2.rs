// SPDX-License-Identifier: CC0-1.0

//! Whether a VPC placement's security groups belong to its VPC is decided from what the
//! service that OWNS them answered — and whether an identifier could name a group at all is
//! decided from the id space itself.
//!
//! RULE: `VPCOptions$SecurityGroupIds` names security groups of the service that owns them,
//! and the VPC a group governs traffic in is that service's to say — so the refusal a
//! placement earns for naming a group of ANOTHER VPC than its subnets, and the refusal it
//! earns for naming a group the account does not hold, are both grounded in what that service
//! ANSWERED, never in the number of the identifiers a request carried. An emulator with no
//! such service mounted resolves neither side, so every call that carries well-formed
//! identifiers is served and the placement keeps exactly the groups it was given:
//! `CreateDomain` and `UpdateDomainConfig` for a domain, `CreateVpcEndpoint` and
//! `UpdateVpcEndpoint` for a service-managed endpoint. The other half of the rule — where the
//! groups and the subnets ARE resolvable, so a cross-VPC pair is refused — is driven by the
//! router's own `opensearch_vpc_security_group_membership` suite, and the existence half by
//! its `opensearch_vpc_endpoint_security_groups_exist` suite, both of which mount the owning
//! service beside this one.
//!
//! The FORM of an identifier is the one thing no answer is needed for
//! (`a_value_that_is_no_security_group_identifier_is_refused_by_every_call` below): that
//! service issues a group id as `sg-` followed by 8 or 17 hexadecimal digits, so a value
//! outside that form names no security group in any account and the call is refused
//! synchronously with the modelled `ValidationException` (HTTP 400) stating
//! `Invalid id: "<value>" (expecting "sg-...")`, creating and changing nothing.
//!
//! The rows differ along what the rule generalises over: how many groups a request names,
//! which of the two identifier forms the owning service mints they take, whether one is
//! repeated, whether the request names groups at all, and — for the form rule — every way a
//! value can fall outside the id space (its prefix, its width, its alphabet and its case) and
//! where in a list it sits.

use serde_json::{json, Value};
use service_com_amazonaws_opensearch_amazonopensearchservice::state::VPC_ENDPOINT_STATUS_ACTIVE;

use super::domain_configuration_rules::assert_no_domain;
use super::domain_lifecycle::{error_code, poll_until, settled_domain, Answer, Emulator};
use super::vpc_endpoint_updates::update_endpoint;
use super::vpc_endpoints::{
    create_endpoint, describe_endpoints, list_endpoints, member, minted, vpc_domain,
};

/// The subnet every placement below names: nothing resolves it either, so what differs
/// between the rows is only the security groups. ONE subnet, because the clusters below ask
/// for no zone awareness and a VPC-access domain names one subnet per Availability Zone it
/// is placed in.
const SUBNETS: [&str; 1] = ["subnet-0ccc0000000000001"];

/// One list of security groups a call names.
struct Row {
    case: &'static str,
    /// The domain this row's create makes, which is also what its read addresses.
    name: &'static str,
    /// The groups the request names; empty names none at all.
    groups: &'static [&'static str],
}

/// The table: the identifier forms the owning service mints a group id in (`sg-` and 8 or 17
/// hexadecimal digits), several counts of them, a repeat, and the absence of any.
const ROWS: &[Row] = &[
    Row {
        case: "one group in the long identifier form",
        name: "sg-ungrounded-one",
        groups: &["sg-0ddd0000000000001"],
    },
    Row {
        case: "one group in the short identifier form",
        name: "sg-ungrounded-short",
        groups: &["sg-0ddd0001"],
    },
    Row {
        case: "three groups, both identifier forms among them",
        name: "sg-ungrounded-three",
        groups: &["sg-0ddd0000000000002", "sg-0ddd0002", "sg-0ddd0000000000003"],
    },
    Row {
        case: "one group named twice",
        name: "sg-ungrounded-repeated",
        groups: &["sg-0ddd0000000000004", "sg-0ddd0000000000004"],
    },
    Row {
        case: "no security group at all",
        name: "sg-ungrounded-none",
        groups: &[],
    },
];

/// The `VPCOptions` a row's call carries.
fn vpc_options(groups: &[&str]) -> Value {
    let mut options = json!({ "SubnetIds": SUBNETS });
    if !groups.is_empty() {
        options["SecurityGroupIds"] = json!(groups);
    }
    options
}

/// The security groups a reported `VPCOptions` carries, in the order it reports them.
fn groups_in(vpc_options: &Value) -> Vec<String> {
    vpc_options["SecurityGroupIds"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect()
}

/// The same, for the `VPCOptions` of a reported `DomainStatus`.
fn reported_groups(status: &Value) -> Vec<String> {
    groups_in(&status["VPCOptions"])
}

#[tokio::test]
async fn a_domain_keeps_the_security_groups_nothing_could_resolve() {
    let emulator = Emulator::new();

    for row in ROWS {
        let case = row.case;
        let created = emulator
            .create(json!({
                "DomainName": row.name,
                "EngineVersion": "OpenSearch_2.11",
                "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
                "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
                "VPCOptions": vpc_options(row.groups),
            }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case}: a placement nothing could resolve grounds no refusal, got {} with \
             {:?}",
            created.status, created.body
        );

        // Read the domain back, so what is asserted is the placement the service STORED
        // rather than only what the create echoed.
        let read = emulator.describe(row.name).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected the domain to be readable, got {} with {:?}",
            read.status, read.body
        );
        let expected: Vec<String> = row.groups.iter().map(|id| (*id).to_string()).collect();
        for (source, status) in [
            ("the create", &created.body["DomainStatus"]),
            ("DescribeDomain", &read.body["DomainStatus"]),
        ] {
            assert_eq!(
                reported_groups(status),
                expected,
                "case {case}: {source} must report the groups the request named, got {status:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_change_keeps_the_security_groups_nothing_could_resolve() {
    let emulator = Emulator::new();

    let name = "sg-ungrounded-changed";
    let created = emulator
        .create(json!({
            "DomainName": name,
            "EngineVersion": "OpenSearch_2.11",
            "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
            "VPCOptions": vpc_options(ROWS[0].groups),
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateDomain must be served, got {} with {:?}",
        created.status, created.body
    );

    // The change half of the rule, over the rows that differ in what the groups look like.
    // The domain is idle before each change, which is the condition — not a stretch of time
    // — a client waits for.
    for row in &ROWS[1..] {
        let case = row.case;
        settled_domain(&emulator, name).await;
        let changed = emulator
            .update_config(name, json!({ "VPCOptions": vpc_options(row.groups) }))
            .await;
        assert_eq!(
            changed.status, 200,
            "case {case}: a change nothing could resolve grounds no refusal, got {} with {:?}",
            changed.status, changed.body
        );
        let read = emulator.describe(name).await;
        let expected: Vec<String> = row.groups.iter().map(|id| (*id).to_string()).collect();
        assert_eq!(
            reported_groups(&read.body["DomainStatus"]),
            expected,
            "case {case}: the domain must carry the groups the change named, got {:?}",
            read.body
        );
    }
}

#[tokio::test]
async fn an_endpoint_keeps_the_security_groups_nothing_could_resolve() {
    let emulator = Emulator::new();
    let arn = vpc_domain(&emulator, "sg-ungrounded-endpoint").await;

    // One endpoint, created with the first row's groups, then moved through the rest: every
    // call carries `VpcOptions`, and none of its identifiers resolves anywhere.
    let case = ROWS[0].case;
    let created = create_endpoint(
        &emulator,
        json!({ "DomainArn": arn, "VpcOptions": vpc_options(ROWS[0].groups) }),
    )
    .await;
    let endpoint = minted(&created, case);
    let id = member(&endpoint, "VpcEndpointId", case);
    assert_eq!(
        groups_in(&endpoint["VpcOptions"]),
        vec![ROWS[0].groups[0].to_string()],
        "case {case}: the endpoint must carry the groups the create named, got {endpoint:?}"
    );

    for row in ROWS[1..].iter().filter(|row| !row.groups.is_empty()) {
        let case = row.case;
        // The endpoint is ACTIVE again before the next change, which is the condition a
        // client polls for.
        poll_until(case, || async {
            let read = describe_endpoints(&emulator, &[id.as_str()]).await;
            read.body["VpcEndpoints"][0]["Status"] == json!(VPC_ENDPOINT_STATUS_ACTIVE)
        })
        .await;

        let changed = update_endpoint(&emulator, &id, &vpc_options(row.groups)).await;
        assert_eq!(
            changed.status, 200,
            "case {case}: a change nothing could resolve grounds no refusal, got {} with {:?}",
            changed.status, changed.body
        );
        let read = describe_endpoints(&emulator, &[id.as_str()]).await;
        let reported = read.body["VpcEndpoints"][0]["VpcOptions"].clone();
        let expected: Vec<String> = row.groups.iter().map(|id| (*id).to_string()).collect();
        assert_eq!(
            groups_in(&reported),
            expected,
            "case {case}: the endpoint must carry the groups the change named, got {reported:?}"
        );
    }
}

/// One list of security groups a call naming a MALFORMED identifier carries, and the value
/// the refusal must name.
struct FormRow {
    case: &'static str,
    /// The groups the request names, in the order it names them.
    groups: &'static [&'static str],
    /// The entry the refusal names: the FIRST entry outside the identifier form.
    offending: &'static str,
}

/// The form table: one row per way a value can fall outside the identifier form the owning
/// service issues (`sg-` and 8 or 17 LOWERCASE hexadecimal digits), plus the positions a
/// malformed value can sit in beside well-formed ones.
const FORM_ROWS: &[FormRow] = &[
    FormRow {
        case: "a value that is no identifier at all",
        groups: &["not-a-security-group!!"],
        offending: "not-a-security-group!!",
    },
    FormRow {
        case: "another resource's identifier",
        groups: &["subnet-0eee0000000000004"],
        offending: "subnet-0eee0000000000004",
    },
    FormRow {
        case: "the prefix with nothing after it",
        groups: &["sg-"],
        offending: "sg-",
    },
    FormRow {
        case: "fewer digits than the short form",
        groups: &["sg-0eee00"],
        offending: "sg-0eee00",
    },
    FormRow {
        case: "a width between the two forms",
        groups: &["sg-0eee000000000000"],
        offending: "sg-0eee000000000000",
    },
    FormRow {
        case: "more digits than the long form",
        groups: &["sg-0eee00000000000012"],
        offending: "sg-0eee00000000000012",
    },
    FormRow {
        case: "a digit outside hexadecimal",
        groups: &["sg-0eee000000000000z"],
        offending: "sg-0eee000000000000z",
    },
    FormRow {
        case: "the prefix in the wrong case",
        groups: &["SG-0eee0000000000005"],
        offending: "SG-0eee0000000000005",
    },
    FormRow {
        case: "the digits in uppercase hexadecimal",
        groups: &["sg-0EEE0000000000007"],
        offending: "sg-0EEE0000000000007",
    },
    FormRow {
        case: "the digits with no prefix",
        groups: &["0eee0000000000006"],
        offending: "0eee0000000000006",
    },
    FormRow {
        case: "a malformed value after a well-formed one",
        groups: &["sg-0eee0000000000002", "sg-0eee000000000000y"],
        offending: "sg-0eee000000000000y",
    },
    FormRow {
        case: "a malformed value before a well-formed one",
        groups: &["sg-zzzzzzzz", "sg-0eee0000000000002"],
        offending: "sg-zzzzzzzz",
    },
    FormRow {
        case: "two malformed values, the first of them named",
        groups: &["sg-0eee0", "not-an-id"],
        offending: "sg-0eee0",
    },
];

/// Identifiers of the FORM the owning service issues — the long and the short one — used
/// wherever a form case needs a value the rule admits.
const WELL_FORMED: [&str; 2] = ["sg-0eee0000000000002", "sg-0eee0003"];

/// The sentence the service refuses a value outside the identifier form with. Spelled out
/// here rather than taken from the crate, so a change to either part fails this suite.
fn malformed_message(value: &str) -> String {
    format!("Invalid id: \"{value}\" (expecting \"sg-...\")")
}

/// Assert `answer` is the modelled refusal naming `offending`.
fn assert_malformed(answer: &Answer, offending: &str, case: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case}: expected 400, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        error_code(answer),
        "ValidationException",
        "case {case}: expected ValidationException, got {:?}",
        answer.body
    );
    assert_eq!(
        answer.body["message"].as_str(),
        Some(malformed_message(offending).as_str()),
        "case {case}: expected the sentence naming {offending:?}, got {:?}",
        answer.body
    );
}

/// A `CreateDomain` body for `name` whose placement names `groups`.
fn form_create_body(name: &str, groups: &[&str]) -> Value {
    json!({
        "DomainName": name,
        "EngineVersion": "OpenSearch_2.11",
        "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
        "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
        "VPCOptions": vpc_options(groups),
    })
}

#[tokio::test]
async fn a_value_that_is_no_security_group_identifier_is_refused_by_every_call() {
    let emulator = Emulator::new();

    // THE DOMAIN half: a create naming a value outside the id space leaves no domain, and one
    // naming only well-formed identifiers is served.
    for (index, row) in FORM_ROWS.iter().enumerate() {
        let case = row.case;
        let name = format!("sg-form-create-{index}");
        let answer = emulator.create(form_create_body(&name, row.groups)).await;
        assert_malformed(&answer, row.offending, &format!("CreateDomain({case})"));
        assert_no_domain(&emulator, &name).await;
    }
    let served = "sg-form-create-served";
    let answer = emulator.create(form_create_body(served, &WELL_FORMED)).await;
    assert_eq!(
        answer.status, 200,
        "case CreateDomain(well-formed identifiers): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    let read = emulator.describe(served).await;
    let well_formed: Vec<String> = WELL_FORMED.iter().map(|id| (*id).to_string()).collect();
    assert_eq!(
        reported_groups(&read.body["DomainStatus"]),
        well_formed,
        "case CreateDomain(well-formed identifiers): the domain must carry the groups the \
         request named, got {:?}",
        read.body
    );

    // ... and the CHANGE half, on a domain whose placement is settled: every refused change
    // leaves the stored groups standing.
    settled_domain(&emulator, served).await;
    for row in FORM_ROWS {
        let case = row.case;
        let answer = emulator
            .update_config(served, json!({ "VPCOptions": vpc_options(row.groups) }))
            .await;
        assert_malformed(&answer, row.offending, &format!("UpdateDomainConfig({case})"));
        let read = emulator.describe(served).await;
        assert_eq!(
            reported_groups(&read.body["DomainStatus"]),
            well_formed,
            "case UpdateDomainConfig({case}): a refused change must leave the placement \
             standing, got {:?}",
            read.body
        );
    }
}

#[tokio::test]
async fn a_vpc_endpoint_call_refuses_a_value_that_is_no_security_group_identifier() {
    let emulator = Emulator::new();
    let arn = vpc_domain(&emulator, "sg-form-endpoint").await;

    // Every create is refused and mints nothing, so the account still holds no endpoint when
    // the served one below is created.
    for row in FORM_ROWS {
        let case = row.case;
        let answer = create_endpoint(
            &emulator,
            json!({ "DomainArn": arn, "VpcOptions": vpc_options(row.groups) }),
        )
        .await;
        assert_malformed(&answer, row.offending, &format!("CreateVpcEndpoint({case})"));
        let listed = list_endpoints(&emulator, None).await;
        assert_eq!(
            listed.body["VpcEndpointSummaryList"]
                .as_array()
                .map_or(0, Vec::len),
            0,
            "case CreateVpcEndpoint({case}): a refused create must mint no endpoint, got {:?}",
            listed.body
        );
    }

    let case = "the endpoint every change below is applied to";
    let created = create_endpoint(
        &emulator,
        json!({ "DomainArn": arn, "VpcOptions": vpc_options(&WELL_FORMED[..1]) }),
    )
    .await;
    let endpoint = minted(&created, case);
    let id = member(&endpoint, "VpcEndpointId", case);

    // The endpoint rests in ACTIVE before the rows below, which is the CONDITION a client
    // polls for: a status the clock still owes a move would change under the rows on its own,
    // and what each row asserts is that a REFUSED change moves nothing.
    poll_until(case, || async {
        let read = describe_endpoints(&emulator, &[id.as_str()]).await;
        read.body["VpcEndpoints"][0]["Status"] == json!(VPC_ENDPOINT_STATUS_ACTIVE)
    })
    .await;
    let settled = describe_endpoints(&emulator, &[id.as_str()]).await;
    let held = settled.body["VpcEndpoints"][0]["VpcOptions"].clone();

    for row in FORM_ROWS {
        let case = row.case;
        let answer = update_endpoint(&emulator, &id, &vpc_options(row.groups)).await;
        assert_malformed(&answer, row.offending, &format!("UpdateVpcEndpoint({case})"));
        let read = describe_endpoints(&emulator, &[id.as_str()]).await;
        assert_eq!(
            read.body["VpcEndpoints"][0]["VpcOptions"], held,
            "case UpdateVpcEndpoint({case}): a refused change must leave the endpoint \
             standing, got {:?}",
            read.body
        );
        assert_eq!(
            read.body["VpcEndpoints"][0]["Status"],
            json!(VPC_ENDPOINT_STATUS_ACTIVE),
            "case UpdateVpcEndpoint({case}): a refused change must leave the endpoint's \
             status alone, got {:?}",
            read.body
        );
    }

    // ... and a change naming only well-formed identifiers is served and applied.
    let changed = update_endpoint(&emulator, &id, &vpc_options(&WELL_FORMED)).await;
    assert_eq!(
        changed.status, 200,
        "case UpdateVpcEndpoint(well-formed identifiers): expected 200, got {} with {:?}",
        changed.status, changed.body
    );
    assert_eq!(
        groups_in(&changed.body["VpcEndpoint"]["VpcOptions"]),
        WELL_FORMED
            .iter()
            .map(|id| (*id).to_string())
            .collect::<Vec<String>>(),
        "case UpdateVpcEndpoint(well-formed identifiers): the endpoint must carry the groups \
         the change named, got {:?}",
        changed.body
    );
}
