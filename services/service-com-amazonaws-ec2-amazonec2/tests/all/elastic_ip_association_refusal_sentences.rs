// SPDX-License-Identifier: CC0-1.0

//! WHICH SENTENCE `AssociateAddress` words an id it cannot resolve with, driven over the
//! wire the service speaks (`ec2Query`, a form-encoded POST answered with XML) through the
//! crate's shared [`super::ec2_query`] harness.
//!
//! RULE: an operation that BINDS a resource to an instance — the Elastic IP association
//! surface, beside `AttachVolume`, `AttachNetworkInterface` and `DetachVolume`, which word
//! it the same way (`src/ops/attach_volume.rs`, `src/ops/attach_network_interface.rs`,
//! `src/ops/detach_volume.rs`) — words an INSTANCE id it cannot read with the request
//! boundary's own template, `Invalid id: "<value>"`
//! (`crate::state::invalid_id_message`, which this operation already answers for the
//! `AllocationId` it resolves), under `InvalidInstanceID.Malformed`. The per-resource
//! sentence "The instance ID '<id>' is malformed" belongs to the operations that act ON the
//! instance itself (`StopInstances`, `GetPasswordData`, `ReportInstanceStatus`), each on
//! its own recorded refusal. A readable id the account holds no instance for is the
//! not-found half and keeps its own sentence, "The instance ID '<id>' does not exist".
//!
//! The ids each member is named with are a different reading
//! (`crate::state::invalid_network_interface_id_malformed` keeps the interface's own
//! sentence on its own recorded evidence), so the rows below carry the member, the id and
//! the sentence the refusal owes, and the table spans what the rule generalises over: WHICH
//! member names the id, WHETHER the value is outside the two forms EC2 issues or inside
//! them and unheld, and HOW it is outside them (the digit count, the leading digit, the
//! case of the digits, the all-zero placeholder, another kind's prefix, free text).
//!
//! Every refusal is read back as the error envelope's own `<Code>` and `<Message>`, and the
//! suite closes with the association the same operation SERVES for a real instance, read
//! back through `DescribeAddresses` — so a handler that refused everything, or that stored
//! nothing, could not pass.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query;

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The image every launch here uses; `RunInstances` only needs one that resolves.
const AMI: &str = "ami-0123456789abcdef0";

/// The code EC2 answers for a value that is not an instance id at all.
const INSTANCE_MALFORMED: &str = "InvalidInstanceID.Malformed";

/// The code EC2 answers for an instance id the account holds no instance for.
const INSTANCE_NOT_FOUND: &str = "InvalidInstanceID.NotFound";

/// The code EC2 answers for a value that is not an allocation id at all.
const ALLOCATION_MALFORMED: &str = "InvalidAllocationID.Malformed";

/// The code EC2 answers for a value that is not a network interface id at all.
const INTERFACE_MALFORMED: &str = "InvalidNetworkInterfaceId.Malformed";

/// The request boundary's own sentence for an id this service could not read: the fixed
/// prefix and the value quoted back (`crate::state::invalid_id_message`).
fn boundary_sentence(id: &str) -> String {
    format!("Invalid id: \"{id}\"")
}

/// The sentence a resource kind words an unreadable id of its own with — the kind and the
/// value in single quotes.
fn kind_sentence(kind: &str, id: &str) -> String {
    format!("The {kind} ID '{id}' is malformed")
}

/// One row: what is notable about the value, the member it is sent in, the value, and the
/// code and sentence the refusal owes.
struct Row {
    case: &'static str,
    member: &'static str,
    value: &'static str,
    code: &'static str,
    message: String,
}

/// Every row of the refusal table: the instance-id half of the rule first, then the two
/// other id members of the same call, whose own readings the rule does not change.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an instance id of the long form with a leading digit above zero",
            member: "InstanceId",
            value: "i-a0000000000000001",
            code: INSTANCE_MALFORMED,
            message: boundary_sentence("i-a0000000000000001"),
        },
        Row {
            case: "an instance id of the long form whose every digit is above zero",
            member: "InstanceId",
            value: "i-9876543210fedcba9",
            code: INSTANCE_MALFORMED,
            message: boundary_sentence("i-9876543210fedcba9"),
        },
        Row {
            case: "the all-zero long instance id, the placeholder every client reaches for",
            member: "InstanceId",
            value: "i-00000000000000000",
            code: INSTANCE_MALFORMED,
            message: boundary_sentence("i-00000000000000000"),
        },
        Row {
            case: "the all-zero short instance id",
            member: "InstanceId",
            value: "i-00000000",
            code: INSTANCE_MALFORMED,
            message: boundary_sentence("i-00000000"),
        },
        Row {
            case: "an instance id one digit short of the long form",
            member: "InstanceId",
            value: "i-0a1b2c3d4e5f6071",
            code: INSTANCE_MALFORMED,
            message: boundary_sentence("i-0a1b2c3d4e5f6071"),
        },
        Row {
            case: "an instance id one digit past the short form",
            member: "InstanceId",
            value: "i-0a1b2c3d4",
            code: INSTANCE_MALFORMED,
            message: boundary_sentence("i-0a1b2c3d4"),
        },
        Row {
            case: "an instance id in upper-case hexadecimal digits",
            member: "InstanceId",
            value: "i-0A1B2C3D",
            code: INSTANCE_MALFORMED,
            message: boundary_sentence("i-0A1B2C3D"),
        },
        Row {
            case: "a digit that is not hexadecimal in an instance id",
            member: "InstanceId",
            value: "i-0a1b2c3z",
            code: INSTANCE_MALFORMED,
            message: boundary_sentence("i-0a1b2c3z"),
        },
        Row {
            case: "another resource kind's prefix in instanceId",
            member: "InstanceId",
            value: "vol-0a1b2c3d4e5f60718",
            code: INSTANCE_MALFORMED,
            message: boundary_sentence("vol-0a1b2c3d4e5f60718"),
        },
        Row {
            case: "free text in instanceId",
            member: "InstanceId",
            value: "not-an-instance",
            code: INSTANCE_MALFORMED,
            message: boundary_sentence("not-an-instance"),
        },
        // The other half of the same rule: a value INSIDE a form EC2 issues is looked up,
        // and an account holding no instance for it is told the instance is not there.
        Row {
            case: "a long-form instance id the account holds no instance for",
            member: "InstanceId",
            value: "i-0aaaaaaaaaaaaaaaa",
            code: INSTANCE_NOT_FOUND,
            message: "The instance ID 'i-0aaaaaaaaaaaaaaaa' does not exist".to_string(),
        },
        Row {
            case: "a short-form instance id the account holds no instance for",
            member: "InstanceId",
            value: "i-0a1b2c3d",
            code: INSTANCE_NOT_FOUND,
            message: "The instance ID 'i-0a1b2c3d' does not exist".to_string(),
        },
        // The allocation id of the SAME call: the template the instance id above now
        // shares, which is why one call no longer words its two ids two ways.
        Row {
            case: "an allocation id one digit short of the short form",
            member: "AllocationId",
            value: "eipalloc-0a1b2c3",
            code: ALLOCATION_MALFORMED,
            message: boundary_sentence("eipalloc-0a1b2c3"),
        },
        Row {
            case: "free text in allocationId",
            member: "AllocationId",
            value: "the-first-one",
            code: ALLOCATION_MALFORMED,
            message: boundary_sentence("the-first-one"),
        },
        // The network interface id keeps its own kind's sentence, on its own recorded
        // evidence: the rule is about the sentence an operation words an id kind with, not
        // about collapsing every kind onto one.
        Row {
            case: "a network interface id one digit short of the short form",
            member: "NetworkInterfaceId",
            value: "eni-0a1b2c3",
            code: INTERFACE_MALFORMED,
            message: kind_sentence("interface", "eni-0a1b2c3"),
        },
        Row {
            case: "another resource kind's prefix in networkInterfaceId",
            member: "NetworkInterfaceId",
            value: "subnet-0123456789abcdef0",
            code: INTERFACE_MALFORMED,
            message: kind_sentence("interface", "subnet-0123456789abcdef0"),
        },
    ]
}

/// RULE: every id `AssociateAddress` cannot resolve is refused with its own code AND the
/// sentence that code carries for this operation — and nothing is associated.
#[tokio::test]
async fn associate_address_words_every_unresolvable_id_in_its_own_sentence() {
    let router = ec2_query::emulator().0;
    // One address the account really holds, so a row that names a bad INSTANCE is refused
    // for the instance rather than for the address, and one live instance, so a row that
    // names a bad ADDRESS is refused for the address.
    let (allocation_id, _public_ip) = allocate(&router).await;
    let instance_id = launch(&router).await;

    for row in rows() {
        let params = request(&row, &allocation_id, &instance_id);
        let (status, body) = ec2_query::call(&router, REGION, "AssociateAddress", &params).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected HTTP 400, got {status} with body {body}",
            row.case
        );
        let code = ec2_query::error_code(&body);
        assert_eq!(
            code, row.code,
            "case {:?}: expected the error code {}, got {code:?} with body {body}",
            row.case, row.code
        );
        let message = ec2_query::error_message(&body);
        assert_eq!(
            message, row.message,
            "case {:?}: expected the message {:?}, got {message:?}",
            row.case, row.message
        );
    }

    // Nothing above associated the address: the refusals are refusals, not writes.
    let association = stored_association(&router, "after every refusal", &allocation_id).await;
    assert_eq!(
        association, None,
        "case \"after every refusal\": the address must still be unassociated, \
         got {association:?}"
    );

    // The same operation SERVES the instance the account does hold, and the association it
    // mints reads back off the stored address — so the sentences above are the answer to the
    // ids, not to the operation.
    let params = vec![
        ("AllocationId".to_string(), allocation_id.clone()),
        ("InstanceId".to_string(), instance_id.clone()),
    ];
    let (status, body) = ec2_query::call(&router, REGION, "AssociateAddress", &params).await;
    assert_eq!(
        status, 200,
        "case \"the instance the account holds\": AssociateAddress must succeed, \
         got {status} with body {body}"
    );
    let minted = ec2_query::element(&body, "associationId");
    assert!(
        minted.as_deref().is_some_and(|id| !id.is_empty()),
        "case \"the instance the account holds\": the association must carry an id, \
         got {body}"
    );
    let stored = stored_association(&router, "the instance the account holds", &allocation_id).await;
    assert_eq!(
        stored, minted,
        "case \"the instance the account holds\": DescribeAddresses must report the \
         association that was minted ({minted:?}), got {stored:?}"
    );
}

/// The request one row sends: the row's own member carries its value, and the members it
/// does not name carry the resources the account really holds, so exactly one id is bad.
fn request(row: &Row, allocation_id: &str, instance_id: &str) -> Vec<(String, String)> {
    let mut params = vec![(row.member.to_string(), row.value.to_string())];
    if row.member != "AllocationId" {
        params.push(("AllocationId".to_string(), allocation_id.to_string()));
    }
    if row.member == "AllocationId" {
        params.push(("InstanceId".to_string(), instance_id.to_string()));
    }
    params
}

/// Allocate one Elastic IP address and answer `(allocation id, public ip)`.
async fn allocate(router: &MultiServiceRouter) -> (String, String) {
    let (status, body) = ec2_query::call(router, REGION, "AllocateAddress", &[]).await;
    assert_eq!(
        status, 200,
        "AllocateAddress must succeed, got {status} with body {body}"
    );
    let allocation_id = ec2_query::element(&body, "allocationId")
        .unwrap_or_else(|| panic!("AllocateAddress must mint an allocation id: {body}"));
    let public_ip = ec2_query::element(&body, "publicIp")
        .unwrap_or_else(|| panic!("AllocateAddress must mint an address: {body}"));
    (allocation_id, public_ip)
}

/// Launch one instance and answer its id.
async fn launch(router: &MultiServiceRouter) -> String {
    let params = vec![
        ("ImageId".to_string(), AMI.to_string()),
        ("MinCount".to_string(), "1".to_string()),
        ("MaxCount".to_string(), "1".to_string()),
    ];
    let (status, body) = ec2_query::call(router, REGION, "RunInstances", &params).await;
    assert_eq!(
        status, 200,
        "RunInstances must succeed, got {status} with body {body}"
    );
    ec2_query::element(&body, "instanceId")
        .unwrap_or_else(|| panic!("RunInstances must report the instance it minted: {body}"))
}

/// The association id `DescribeAddresses` reports for `allocation_id`, if any.
async fn stored_association(
    router: &MultiServiceRouter,
    case: &str,
    allocation_id: &str,
) -> Option<String> {
    let params = vec![("AllocationId.1".to_string(), allocation_id.to_string())];
    let (status, body) = ec2_query::call(router, REGION, "DescribeAddresses", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAddresses must report {allocation_id}, got {status} with \
         body {body}"
    );
    ec2_query::element(&body, "associationId")
}
