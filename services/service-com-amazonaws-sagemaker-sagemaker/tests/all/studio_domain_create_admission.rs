// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for the two things SageMaker settles about a `CreateDomain`
//! request before it mints a Studio domain: whether the account already holds a domain
//! under the `DomainName`, and whether the network identifiers the request names are
//! written in the form Amazon EC2 mints them in.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, so what
//! these tests exercise is what a real SDK client gets. The harness (`Emulator`,
//! `error_code`, `message`, `str_field`) is the one `inference_deployment_lifecycle`
//! already defines for this target.
//!
//! The rules under test:
//!
//! * a `DomainName` this account already holds a domain under, in the Region the request is
//!   signed for, is refused `ResourceInUse`/400 with the sentence that names it and no
//!   second domain is minted — while the same name in ANOTHER Region is free, and the name
//!   is free again once `DeleteDomain` has taken the domain that held it away;
//! * a `VpcId` or a `SubnetIds` entry that is not written in EC2's published identifier
//!   form is refused `ValidationException`/400 with one clause per offending member, and no
//!   domain is stored.
//!
//! Both tables span what the rules generalise over rather than one recorded call: the name
//! shapes `DomainName`'s pattern admits, the Region the request is signed for, the status
//! the held domain is in, which of the two network members is malformed, how it is
//! malformed, and how many entries a subnet list carries.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::inference_deployment_lifecycle::{error_code, message, str_field, Emulator};

/// An execution role inside `RoleArn`'s modelled pattern, for the `DefaultUserSettings`
/// every create must carry.
const EXECUTION_ROLE: &str = "arn:aws:iam::123456789012:role/service-role/StudioRole";

/// How long a poll below gives the clock to complete a teardown it already owes: far longer
/// than the span the state gives one, so a loaded host still gets there well inside it. No
/// case here tests how long a teardown takes.
const TEARDOWN_DEADLINE: Duration = Duration::from_secs(120);

/// How often a poll looks again. Only the loop's condition decides when it stops.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The smallest `CreateDomain` request: the three required members and nothing else.
fn minimal(domain_name: &str) -> Value {
    json!({
        "DomainName": domain_name,
        "AuthMode": "IAM",
        "DefaultUserSettings": { "ExecutionRole": EXECUTION_ROLE }
    })
}

/// [`minimal`] plus the network members a row names.
fn with_network(domain_name: &str, vpc_id: Option<&str>, subnet_ids: &[&str]) -> Value {
    let mut request = minimal(domain_name);
    if let Some(vpc_id) = vpc_id {
        request["VpcId"] = json!(vpc_id);
    }
    if !subnet_ids.is_empty() {
        request["SubnetIds"] = json!(subnet_ids);
    }
    request
}

/// The sentence `CreateDomain` refuses a name the account already holds with. A literal on
/// purpose: this suite is what pins the wording, so it does not ask the implementation what
/// it is. The name is the one the request sent.
fn already_exists(domain_name: &str) -> String {
    format!("The Domain Name '{domain_name}' already exists in this account in this region.")
}

/// The clause a malformed `VpcId` is refused with: the value the request sent, the member it
/// was sent at, and the identifier form it had to be written in. A literal, for the same
/// reason [`already_exists`] is one.
fn vpc_id_clause(vpc_id: &str) -> String {
    format!(
        "Value '{vpc_id}' at 'vpcId' failed to satisfy constraint: Member must satisfy \
         regular expression pattern: vpc-[a-z0-9]{{8,}}"
    )
}

/// The clause a malformed `SubnetIds` is refused with: the WHOLE list the request sent, at
/// the list member, and the element shape's constraint set — its modelled `@length` bounds
/// and the identifier form — nested inside it.
fn subnet_ids_clause(subnet_ids: &[&str]) -> String {
    format!(
        "Value '[{}]' at 'subnetIds' failed to satisfy constraint: Member must satisfy \
         constraint: [Member must have length less than or equal to 32, Member must have \
         length greater than or equal to 0, Member must satisfy regular expression pattern: \
         subnet-[a-z0-9]{{8,}}]",
        subnet_ids.join(", ")
    )
}

/// AWS's envelope over the clauses of one refusal: `1 validation error detected: <clause>`
/// for one, `<N> validation errors detected: <clause>; <clause>` for several.
fn validation_envelope(clauses: &[String]) -> String {
    match clauses {
        [only] => format!("1 validation error detected: {only}"),
        many => format!(
            "{} validation errors detected: {}",
            many.len(),
            many.join("; ")
        ),
    }
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

/// Read `DescribeDomain` for `domain_id` in `region` until it no longer resolves, i.e. the
/// teardown the clock owed is complete.
///
/// Waits for the CONDITION, never for a fixed span: the reader is asked again until it
/// refuses the id or a generous deadline passes.
async fn domain_gone(emulator: &Emulator, case: &str, region: &str, domain_id: &str) {
    let request = json!({ "DomainId": domain_id });
    let deadline = Instant::now() + TEARDOWN_DEADLINE;
    loop {
        let (status, parsed) = emulator.call_in(region, "DescribeDomain", &request).await;
        if status == 400 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the domain must be gone once the clock completes the teardown, \
             last read {status} {parsed}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a `DomainName` this account already holds a domain under, in the Region the request
/// is signed for, is refused `ResourceInUse`/400 with the sentence naming it and nothing is
/// stored for the refusal — so exactly one domain of the account holds that name — while the
/// same name in another Region is free, and the name is free again once `DeleteDomain` has
/// taken the domain that held it away.
#[tokio::test]
async fn create_domain_refuses_a_name_this_account_already_holds() {
    // The rows differ along what the rule generalises over: the shapes `DomainName`'s
    // modelled pattern admits (one character, hyphenated, doubly hyphenated, digits only,
    // the 63-character maximum) and the Region the pair of creates is signed for.
    let longest_name = format!("a{}", "b".repeat(62));
    let rows: Vec<(&str, &str, &str, &str)> = vec![
        ("a plain name", "us-east-1", "eu-west-1", "studio"),
        ("a single-character name", "eu-central-1", "us-west-2", "a"),
        ("a hyphenated name", "ap-southeast-2", "us-east-2", "studio-one"),
        ("a doubly hyphenated name", "us-west-1", "eu-west-3", "studio--one"),
        ("digits only", "ca-central-1", "us-east-1", "2024"),
        (
            "the longest name the length bound admits",
            "eu-north-1",
            "us-east-1",
            longest_name.as_str(),
        ),
    ];

    for (case, region, elsewhere, name) in rows {
        let emulator = Emulator::new();
        let request = minimal(name);

        let created = emulator.call_in(region, "CreateDomain", &request).await;
        assert_eq!(
            created.0, 200,
            "case {case:?}: the first create must be served, got {} with {}",
            created.0, created.1
        );
        let first = str_field(&created.1, "DomainId");

        // The identical call, while the first domain holds the name.
        let (status, refused) = emulator.call_in(region, "CreateDomain", &request).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "ResourceInUse"),
            "case {case:?}: a name the account holds must be refused ResourceInUse, got \
             {status} with {refused}"
        );
        assert_eq!(
            message(&refused),
            already_exists(name),
            "case {case:?}: expected the in-use sentence naming the name, got {:?}",
            message(&refused)
        );
        assert!(
            refused.get("DomainId").is_none() && refused.get("DomainArn").is_none(),
            "case {case:?}: a refused create mints nothing, got {refused}"
        );

        // The same name in ANOTHER Region names no domain of that Region, so it is free:
        // the invariant is one domain per name per Region.
        let (status, other_region) = emulator
            .call_in(elsewhere, "CreateDomain", &request)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the name must be free in {elsewhere}, got {status} with \
             {other_region}"
        );
        let second = str_field(&other_region, "DomainId");
        assert_ne!(
            second, first,
            "case {case:?}: every served create mints its own DomainId, got the repeat \
             {second}"
        );

        // Two served creates, two domains, and no third one from the refusal.
        let listed = emulator
            .call_in(region, "ListDomains", &json!({ "MaxResults": 100 }))
            .await;
        let ids = listed_ids(&listed.1);
        assert_eq!(
            ids.len(),
            2,
            "case {case:?}: expected the two served creates' domains, got {}",
            listed.1
        );

        // Once the clock has completed the teardown the delete asked for, the name is free
        // again and the create that was refused is served, minting a NEW domain.
        let (status, deleted) = emulator
            .call_in(region, "DeleteDomain", &json!({ "DomainId": first }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the delete must be served, got {status} with {deleted}"
        );
        domain_gone(&emulator, case, region, &first).await;

        let (status, again) = emulator.call_in(region, "CreateDomain", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the name must be free once the domain that held it is gone, got \
             {status} with {again}"
        );
        let third = str_field(&again, "DomainId");
        assert!(
            third != first && third != second,
            "case {case:?}: the re-created domain is a new one, got {third}"
        );
    }
}

/// One row of the identifier-form table: the network members a create names, and the clauses
/// the refusal has to state — empty for a request every identifier of which is in the form.
struct NetworkRow {
    case: &'static str,
    vpc_id: Option<&'static str>,
    subnet_ids: Vec<&'static str>,
    clauses: Vec<String>,
}

/// RULE: a `VpcId` or a `SubnetIds` entry that is not written in the form Amazon EC2 mints
/// an identifier in — the resource's prefix, a hyphen, then at least eight lowercase
/// alphanumerics — is refused `ValidationException`/400 with one clause per offending
/// member, in the order the operation lists its members, and no domain is stored; a request
/// whose identifiers are all in the form is served.
#[tokio::test]
async fn create_domain_refuses_a_network_identifier_no_ec2_could_have_minted() {
    let emulator = Emulator::new();
    let region = "us-west-2";

    // The rows differ along what the rule generalises over: which member is malformed, HOW
    // it is malformed (no prefix, too short a suffix, the wrong case, the other resource's
    // prefix), how many entries the subnet list carries and how many of them are malformed,
    // and the requests that are in the form and must be served.
    let rows: Vec<NetworkRow> = vec![
        NetworkRow {
            case: "no network member at all",
            vpc_id: None,
            subnet_ids: Vec::new(),
            clauses: Vec::new(),
        },
        NetworkRow {
            case: "both members in the published form",
            vpc_id: Some("vpc-0a1b2c3d"),
            subnet_ids: vec!["subnet-0a1b2c3d", "subnet-0a1b2c3d4e5f60718"],
            clauses: Vec::new(),
        },
        NetworkRow {
            case: "a VPC id with no prefix",
            vpc_id: Some("not-a-real-vpc-id"),
            subnet_ids: vec!["subnet-0a1b2c3d"],
            clauses: vec![vpc_id_clause("not-a-real-vpc-id")],
        },
        NetworkRow {
            case: "a VPC id one character short",
            vpc_id: Some("vpc-0a1b2c"),
            subnet_ids: Vec::new(),
            clauses: vec![vpc_id_clause("vpc-0a1b2c")],
        },
        NetworkRow {
            case: "a VPC id in the wrong case",
            vpc_id: Some("vpc-0A1B2C3D"),
            subnet_ids: Vec::new(),
            clauses: vec![vpc_id_clause("vpc-0A1B2C3D")],
        },
        NetworkRow {
            case: "a subnet id at the VPC member",
            vpc_id: Some("subnet-0a1b2c3d"),
            subnet_ids: Vec::new(),
            clauses: vec![vpc_id_clause("subnet-0a1b2c3d")],
        },
        NetworkRow {
            case: "one malformed subnet among well-formed ones",
            vpc_id: Some("vpc-0a1b2c3d"),
            subnet_ids: vec!["subnet-0a1b2c3d", "subnet-1", "subnet-0a1b2c3d4e5f60718"],
            clauses: vec![subnet_ids_clause(&[
                "subnet-0a1b2c3d",
                "subnet-1",
                "subnet-0a1b2c3d4e5f60718",
            ])],
        },
        NetworkRow {
            case: "every subnet malformed",
            vpc_id: Some("vpc-0a1b2c3d"),
            subnet_ids: vec!["one", "two"],
            clauses: vec![subnet_ids_clause(&["one", "two"])],
        },
        NetworkRow {
            case: "both members malformed",
            vpc_id: Some("not-a-real-vpc-id"),
            subnet_ids: vec!["not-a-real-subnet-id"],
            clauses: vec![
                vpc_id_clause("not-a-real-vpc-id"),
                subnet_ids_clause(&["not-a-real-subnet-id"]),
            ],
        },
    ];

    let mut served = 0usize;
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let name = format!("network{index}");
        let request = with_network(&name, row.vpc_id, &row.subnet_ids);
        let (status, answered) = emulator.call_in(region, "CreateDomain", &request).await;

        if row.clauses.is_empty() {
            assert_eq!(
                status, 200,
                "case {case:?}: identifiers in the published form must be served, got \
                 {status} with {answered}"
            );
            served += 1;
            continue;
        }

        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "ValidationException"),
            "case {case:?}: a malformed identifier must be refused 400 ValidationException, \
             got {status} with {answered}"
        );
        assert_eq!(
            message(&answered),
            validation_envelope(&row.clauses),
            "case {case:?}: expected one clause per offending member, got {:?}",
            message(&answered)
        );
        assert!(
            answered.get("DomainId").is_none() && answered.get("DomainArn").is_none(),
            "case {case:?}: a refused create mints nothing, got {answered}"
        );

        // Nothing was persisted for the refusal: the name the refused create carried names
        // no domain of the account.
        let listed = emulator
            .call_in(region, "ListDomains", &json!({ "MaxResults": 100 }))
            .await;
        let names: Vec<String> = listed
            .1
            .get("Domains")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: expected a Domains list in {}", listed.1))
            .iter()
            .map(|details| str_field(details, "DomainName"))
            .collect();
        assert!(
            !names.contains(&name),
            "case {case:?}: a refused create stores nothing, got {names:?}"
        );
    }

    // Exactly the served creates are stored, so every refusal above wrote nothing.
    let listed = emulator
        .call_in(region, "ListDomains", &json!({ "MaxResults": 100 }))
        .await;
    assert_eq!(
        listed_ids(&listed.1).len(),
        served,
        "expected {served} domains from the served creates, got {}",
        listed.1
    );
}
