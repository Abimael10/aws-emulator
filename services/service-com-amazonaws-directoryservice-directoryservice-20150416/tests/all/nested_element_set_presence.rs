// SPDX-License-Identifier: CC0-1.0

//! The REQUIREMENT a clause stated at a list member nests, driven end to end over HTTP.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: DirectoryService_20150416.<Operation>` and an `awsJson1_1` body
//! (`crate::harness`), so nothing here reaches into the state struct and an answer that is
//! not computed from persisted state cannot pass.
//!
//! The rule under test: a constraint a list's ELEMENTS break is one validation error stated
//! at the LIST member, quoting the WHOLE list that member held and nesting the element
//! shape's declared constraints inside
//! `Member must satisfy constraint: [ … ]` — and that nested set closes with the requirement
//! that the member not be null exactly where the model declares THAT member `@required`.
//! Every other list member — one the model leaves optional, and one carrying a `@default`,
//! which a request may omit altogether — states the element shape's constraints alone.
//!
//! It is asserted over a TABLE spanning what the rule generalises over: the three forms the
//! member's declaration takes (`@required`, `@default`, optional), a list the input declares
//! itself and one inside a structure it nests, element sets of one constraint and of three,
//! lists of one and of several entries, and operations the issues this rule was written
//! against never named — so no row can pass by matching one recorded message, and a renderer
//! that flipped the requirement on for every list, or off for every list, fails here.

use serde_json::{json, Value};

use crate::harness::{call, service};

/// A well-formed `DirectoryId` (`^d-[0-9a-f]{10}$`) this account never creates: every row
/// below is decided before any handler resolves it.
const ABSENT_DIRECTORY_ID: &str = "d-0123456789";

/// The requirement the nested set closes with for a member the model declares `@required`,
/// as the framework words an absent member.
const ENTRY_PRESENT: &str = "Member must not be null";

/// The element expressions under test, read off the model
/// (`smithy_query(query="shape",shape="DirectoryIds,DomainControllerIds,TrustIds,SnapshotIds,TopicNames,CidrIps,DnsIpAddrs,SubnetIds,TagKeys")`).
const DIRECTORY_ID_PATTERN: &str = r"^d-[0-9a-f]{10}$";
const DOMAIN_CONTROLLER_ID_PATTERN: &str = r"^dc-[0-9a-f]{10}$";
const TRUST_ID_PATTERN: &str = r"^t-[0-9a-f]{10}$";
const SNAPSHOT_ID_PATTERN: &str = r"^s-[0-9a-f]{10}$";
const TOPIC_NAME_PATTERN: &str = r"^[a-zA-Z0-9_-]+$";
const IP_ADDR_PATTERN: &str =
    r"^(?:(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.){3}(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)$";
const CIDR_IP_PATTERN: &str = r"^(([0-9]|[1-9][0-9]|1[0-9]{2}|2[0-4][0-9]|25[0-5])\.){3}([0-9]|[1-9][0-9]|1[0-9]{2}|2[0-4][0-9]|25[0-5])(\/([1-9]|[1-2][0-9]|3[0-2]))$";
const SUBNET_ID_PATTERN: &str = r"^(subnet-[0-9a-f]{8}|subnet-[0-9a-f]{17})$";
const TAG_KEY_PATTERN: &str = r"^([\p{L}\p{Z}\p{N}_.:/=+\-@]*)$";

/// One row: the operation, a body with at least one offending entry in ONE list, the member
/// path the clause names, the WHOLE list it quotes, the element shape's declared constraints
/// as the clause nests them, and whether the set closes with [`ENTRY_PRESENT`].
struct NestedSetCase {
    case: &'static str,
    operation: &'static str,
    body: Value,
    path: &'static str,
    quoted: &'static str,
    /// The element shape's whole declared constraint set, as the clause nests it — WITHOUT
    /// the closing presence requirement, which `required` decides.
    element_set: String,
    /// Whether the model declares the list member `@required` — named in the comment beside
    /// each row.
    required: bool,
}

/// The clause's `Member must satisfy regular expression pattern: …` requirement.
fn expression(pattern: &str) -> String {
    format!("Member must satisfy regular expression pattern: {pattern}")
}

/// The rows this rule is asserted over.
fn nested_set_rows() -> Vec<NestedSetCase> {
    vec![
        // `DescribeDirectoriesRequest$DirectoryIds`: optional — "If this member is null, all
        // directories that belong to the current account are returned."
        NestedSetCase {
            case: "a directory listing's optional filter",
            operation: "DescribeDirectories",
            body: json!({ "DirectoryIds": ["d-90000ff0a"] }),
            path: "directoryIds",
            quoted: "d-90000ff0a",
            element_set: expression(DIRECTORY_ID_PATTERN),
            required: false,
        },
        // The same member with several entries, two of which offend: still ONE validation
        // error, quoting the whole list, and still no presence requirement.
        NestedSetCase {
            case: "two offending entries of that same filter",
            operation: "DescribeDirectories",
            body: json!({ "DirectoryIds": ["d-90000ff0a", "d-0000000000", "not-a-directory"] }),
            path: "directoryIds",
            quoted: "d-90000ff0a, d-0000000000, not-a-directory",
            element_set: expression(DIRECTORY_ID_PATTERN),
            required: false,
        },
        // `DescribeDomainControllersRequest$DomainControllerIds`: optional too.
        NestedSetCase {
            case: "a domain controller listing's optional filter",
            operation: "DescribeDomainControllers",
            body: json!({
                "DirectoryId": ABSENT_DIRECTORY_ID,
                "DomainControllerIds": ["dc-00112233"],
            }),
            path: "domainControllerIds",
            quoted: "dc-00112233",
            element_set: expression(DOMAIN_CONTROLLER_ID_PATTERN),
            required: false,
        },
        // `DescribeTrustsRequest$TrustIds`: optional — "If this member is null, all trust
        // relationships that belong to the current account are returned."
        NestedSetCase {
            case: "a trust listing's optional filter",
            operation: "DescribeTrusts",
            body: json!({
                "DirectoryId": ABSENT_DIRECTORY_ID,
                "TrustIds": ["t-00112233", "t-0011223344"],
            }),
            path: "trustIds",
            quoted: "t-00112233, t-0011223344",
            element_set: expression(TRUST_ID_PATTERN),
            required: false,
        },
        // `DescribeSnapshotsRequest$SnapshotIds`: optional.
        NestedSetCase {
            case: "a snapshot listing's optional filter",
            operation: "DescribeSnapshots",
            body: json!({ "SnapshotIds": ["snap-0011223344"] }),
            path: "snapshotIds",
            quoted: "snap-0011223344",
            element_set: expression(SNAPSHOT_ID_PATTERN),
            required: false,
        },
        // `DescribeEventTopicsRequest$TopicNames`: optional, and its element shape declares
        // a `@length` pair BESIDE its `@pattern` — the clause states the whole declared set
        // and still closes it without the presence requirement.
        NestedSetCase {
            case: "a topic listing's optional filter, whose elements carry three constraints",
            operation: "DescribeEventTopics",
            body: json!({
                "DirectoryId": ABSENT_DIRECTORY_ID,
                "TopicNames": ["not a topic name"],
            }),
            path: "topicNames",
            quoted: "not a topic name",
            element_set: format!(
                "Member must have length less than or equal to 256, Member must have length \
                 greater than or equal to 1, {}",
                expression(TOPIC_NAME_PATTERN)
            ),
            required: false,
        },
        // `DirectoryVpcSettings$SubnetIds`: `@required`, inside a structure the request
        // nests.
        NestedSetCase {
            case: "a create's required nested subnets",
            operation: "CreateMicrosoftAD",
            body: json!({
                "Name": "nested-set.example.com",
                "Password": crate::directory_lifecycle::PASSWORD,
                "VpcSettings": {
                    "VpcId": "vpc-0a1b2c3d",
                    "SubnetIds": ["subnet-0a1b2c3d", "subnet-not-hex"],
                },
            }),
            path: "vpcSettings.subnetIds",
            quoted: "subnet-0a1b2c3d, subnet-not-hex",
            element_set: expression(SUBNET_ID_PATTERN),
            required: true,
        },
        // `DirectoryConnectSettings$SubnetIds`: `@required` as well, in another operation's
        // nested settings block, so the requirement follows the declaration and not the
        // operation.
        NestedSetCase {
            case: "an AD Connector's required nested subnets",
            operation: "ConnectDirectory",
            body: json!({
                "Name": "nested-set-connected.example.com",
                "Password": crate::directory_lifecycle::PASSWORD,
                "Size": "Small",
                "ConnectSettings": {
                    "VpcId": "vpc-0a1b2c3d",
                    "SubnetIds": ["subnet-not-hex"],
                    "CustomerDnsIps": ["10.0.0.10"],
                    "CustomerUserName": "Admin",
                },
            }),
            path: "connectSettings.subnetIds",
            quoted: "subnet-not-hex",
            element_set: expression(SUBNET_ID_PATTERN),
            required: true,
        },
        // `AssessmentConfiguration$CustomerDnsIps`: `@required`, inside a structure the
        // request nests — and the SAME member name an AD Connector's connect settings carry
        // a `@default` for, so the requirement follows the declaration of the member the
        // clause is about rather than one answer for the name.
        NestedSetCase {
            case: "an assessment's required nested DNS servers",
            operation: "StartADAssessment",
            body: json!({
                "AssessmentConfiguration": {
                    "CustomerDnsIps": ["10.0.0.10", "999.999.999.999"],
                    "DnsName": "assessed.example.com",
                    "InstanceIds": ["i-0a1b2c3d", "i-1a2b3c4d"],
                    "VpcSettings": {
                        "VpcId": "vpc-0a1b2c3d",
                        "SubnetIds": ["subnet-0a1b2c3d", "subnet-1a2b3c4d"],
                    },
                },
            }),
            path: "assessmentConfiguration.customerDnsIps",
            quoted: "10.0.0.10, 999.999.999.999",
            element_set: expression(IP_ADDR_PATTERN),
            required: true,
        },
        // `RemoveTagsFromResourceRequest$TagKeys`: `@required`, on the input itself, with a
        // `@length` pair beside the `@pattern`.
        NestedSetCase {
            case: "the required tag keys a removal names",
            operation: "RemoveTagsFromResource",
            body: json!({
                "ResourceId": ABSENT_DIRECTORY_ID,
                "TagKeys": ["bad!key"],
            }),
            path: "tagKeys",
            quoted: "bad!key",
            element_set: format!(
                "Member must have length less than or equal to 128, Member must have length \
                 greater than or equal to 1, {}",
                expression(TAG_KEY_PATTERN)
            ),
            required: true,
        },
        // `RemoveIpRoutesRequest$CidrIps`: carries a `@default` of `[]`, which a request may
        // omit altogether, so the model does not require it.
        NestedSetCase {
            case: "the defaulted address blocks a removal names",
            operation: "RemoveIpRoutes",
            body: json!({
                "DirectoryId": ABSENT_DIRECTORY_ID,
                "CidrIps": ["198.51.100.0"],
            }),
            path: "cidrIps",
            quoted: "198.51.100.0",
            element_set: expression(CIDR_IP_PATTERN),
            required: false,
        },
        // `CreateConditionalForwarderRequest$DnsIpAddrs`: a `@default` of `[]` too.
        NestedSetCase {
            case: "a forwarder's defaulted DNS addresses",
            operation: "CreateConditionalForwarder",
            body: json!({
                "DirectoryId": ABSENT_DIRECTORY_ID,
                "RemoteDomainName": "remote.example.com",
                "DnsIpAddrs": ["999.999.999.999"],
            }),
            path: "dnsIpAddrs",
            quoted: "999.999.999.999",
            element_set: expression(IP_ADDR_PATTERN),
            required: false,
        },
        // ...and the same member on the UPDATE, with a good entry beside the bad one.
        NestedSetCase {
            case: "the same defaulted member on the forwarder update",
            operation: "UpdateConditionalForwarder",
            body: json!({
                "DirectoryId": ABSENT_DIRECTORY_ID,
                "RemoteDomainName": "remote.example.com",
                "DnsIpAddrs": ["10.0.0.1", "10.0.0"],
            }),
            path: "dnsIpAddrs",
            quoted: "10.0.0.1, 10.0.0",
            element_set: expression(IP_ADDR_PATTERN),
            required: false,
        },
    ]
}

/// The whole message one row must be refused with.
fn nested_set_message(row: &NestedSetCase) -> String {
    let nested = if row.required {
        format!("{}, {ENTRY_PRESENT}", row.element_set)
    } else {
        row.element_set.clone()
    };
    format!(
        "1 validation error detected: Value '[{}]' at '{}' failed to satisfy constraint: \
         Member must satisfy constraint: [{nested}]",
        row.quoted, row.path
    )
}

/// RULE: the nested set closes with the presence requirement exactly where the model declares
/// the list member `@required`.
#[tokio::test]
async fn the_nested_element_set_follows_the_members_own_declaration() {
    let router = service();

    for row in nested_set_rows() {
        let (status, parsed) = call(&router, row.operation, row.body.clone()).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected the element constraint to refuse the request, got {status} \
             with {parsed}",
            row.case
        );
        assert_eq!(
            parsed["__type"],
            json!("ValidationException"),
            "case {:?}: expected the framework's constraint refusal, got {parsed}",
            row.case
        );
        let message = parsed["message"].as_str().unwrap_or_default();
        assert_eq!(
            message,
            nested_set_message(&row),
            "case {:?}: expected the clause stated at the list member, got {message:?}",
            row.case
        );
        // The presence requirement is nested only where the row says it is, however many
        // entries offend.
        assert_eq!(
            message.contains(ENTRY_PRESENT),
            row.required,
            "case {:?}: expected the presence requirement nested = {}, got {message:?}",
            row.case,
            row.required
        );
    }
}

/// RULE, the same one read the other way round: the requirement is PER MEMBER rather than a
/// property of a dense list, so ONE request whose two offending lists are declared
/// differently — an AD Connector's `@required` subnets beside its `@default` customer DNS
/// addresses, both nested in the same settings block — is refused with two clauses, exactly
/// one of which closes with the presence requirement.
#[tokio::test]
async fn two_lists_of_one_request_state_the_requirement_their_own_declaration_asks_for() {
    let router = service();

    let case = "a required list and a defaulted list of one settings block";
    let (status, parsed) = call(
        &router,
        "ConnectDirectory",
        json!({
            "Name": "two-lists.example.com",
            "Password": crate::directory_lifecycle::PASSWORD,
            "Size": "Small",
            "ConnectSettings": {
                "VpcId": "vpc-0a1b2c3d",
                "SubnetIds": ["subnet-not-hex", "subnet-0a1b2c3d"],
                "CustomerDnsIps": ["999.999.999.999"],
                "CustomerUserName": "Admin",
            },
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected both element constraints to refuse the request, got {status} \
         with {parsed}"
    );
    let message = parsed["message"].as_str().unwrap_or_default();
    assert!(
        message.starts_with("2 validation errors detected: "),
        "case {case:?}: expected one counted clause per member, got {message:?}"
    );
    let required_clause = format!(
        "Value '[subnet-not-hex, subnet-0a1b2c3d]' at 'connectSettings.subnetIds' failed to \
         satisfy constraint: Member must satisfy constraint: [{}, {ENTRY_PRESENT}]",
        expression(SUBNET_ID_PATTERN)
    );
    assert!(
        message.contains(&required_clause),
        "case {case:?}: expected the @required list's clause to close with the presence \
         requirement, got {message:?}"
    );
    let defaulted_clause = format!(
        "Value '[999.999.999.999]' at 'connectSettings.customerDnsIps' failed to satisfy \
         constraint: Member must satisfy constraint: [{}]",
        expression(IP_ADDR_PATTERN)
    );
    assert!(
        message.contains(&defaulted_clause),
        "case {case:?}: expected the defaulted list's clause to state the element expression \
         alone, got {message:?}"
    );
    assert_eq!(
        message.matches(ENTRY_PRESENT).count(),
        1,
        "case {case:?}: expected the presence requirement in exactly one of the two clauses, \
         got {message:?}"
    );
}
