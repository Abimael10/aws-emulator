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
//! that the member not be null exactly where the model makes it one a request always
//! carries: a member declared `@required`, and one carrying a `@default`, which a caller
//! cannot submit as null either. A member the model leaves OPTIONAL — one a request may omit
//! altogether — states the element shape's constraints alone.
//!
//! It is asserted over a TABLE spanning both sides of that decision and the forms a list
//! member takes: two optional filters (a directory listing's and a domain controller
//! listing's), a `@required` member of a structure the request nests, a `@required` member of
//! the input itself, and two members carrying a `@default` — one of the input, one nested —
//! so no row can pass by matching one recorded message.

use serde_json::{json, Value};

use crate::harness::{call, service};

/// A well-formed `DirectoryId` (`^d-[0-9a-f]{10}$`) this account never creates: every row
/// below is decided before any handler resolves it.
const ABSENT_DIRECTORY_ID: &str = "d-0123456789";

/// The requirement the nested set closes with for a member the model never leaves optional,
/// as the framework words an absent member.
const ENTRY_PRESENT: &str = "Member must not be null";

/// One row: the operation, a body with at least one offending entry in ONE list, the member
/// path the clause names, the WHOLE list it quotes, the element shape's declared constraints
/// as the clause nests them, and whether the set closes with [`ENTRY_PRESENT`].
struct NestedSetCase {
    case: &'static str,
    operation: &'static str,
    body: Value,
    path: &'static str,
    quoted: &'static str,
    /// The element shape's `@pattern`, read off the model
    /// (`smithy_query(query="shape",shape="DirectoryIds,DomainControllerIds,SubnetIds,TagKeys,CidrIps,DnsIpAddrs")`).
    element_pattern: &'static str,
    /// Whether the model makes the list member one a request always carries — named in the
    /// comment beside each row.
    carried: bool,
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
            element_pattern: r"^d-[0-9a-f]{10}$",
            carried: false,
        },
        // The same member with several entries, two of which offend: still ONE validation
        // error, quoting the whole list, and still no presence requirement.
        NestedSetCase {
            case: "two offending entries of that same filter",
            operation: "DescribeDirectories",
            body: json!({ "DirectoryIds": ["d-90000ff0a", "d-0000000000", "not-a-directory"] }),
            path: "directoryIds",
            quoted: "d-90000ff0a, d-0000000000, not-a-directory",
            element_pattern: r"^d-[0-9a-f]{10}$",
            carried: false,
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
            element_pattern: r"^dc-[0-9a-f]{10}$",
            carried: false,
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
            element_pattern: r"^(subnet-[0-9a-f]{8}|subnet-[0-9a-f]{17})$",
            carried: true,
        },
        // `RemoveTagsFromResourceRequest$TagKeys`: `@required`, on the input itself.
        NestedSetCase {
            case: "the required tag keys a removal names",
            operation: "RemoveTagsFromResource",
            body: json!({
                "ResourceId": ABSENT_DIRECTORY_ID,
                "TagKeys": ["bad!key"],
            }),
            path: "tagKeys",
            quoted: "bad!key",
            element_pattern: r"^([\p{L}\p{Z}\p{N}_.:/=+\-@]*)$",
            carried: true,
        },
        // `RemoveIpRoutesRequest$CidrIps`: carries a `@default` of `[]`.
        NestedSetCase {
            case: "the defaulted address blocks a removal names",
            operation: "RemoveIpRoutes",
            body: json!({
                "DirectoryId": ABSENT_DIRECTORY_ID,
                "CidrIps": ["198.51.100.0"],
            }),
            path: "cidrIps",
            quoted: "198.51.100.0",
            element_pattern: r"^(([0-9]|[1-9][0-9]|1[0-9]{2}|2[0-4][0-9]|25[0-5])\.){3}([0-9]|[1-9][0-9]|1[0-9]{2}|2[0-4][0-9]|25[0-5])(\/([1-9]|[1-2][0-9]|3[0-2]))$",
            carried: true,
        },
        // `CreateConditionalForwarderRequest$DnsIpAddrs`: carries a `@default` of `[]`.
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
            element_pattern: r"^(?:(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.){3}(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)$",
            carried: true,
        },
    ]
}

/// The whole message one row must be refused with.
fn nested_set_message(row: &NestedSetCase) -> String {
    let mut nested = format!(
        "Member must satisfy regular expression pattern: {}",
        row.element_pattern
    );
    // The tag key's element shape declares a `@length` pair as well, and the clause states
    // the WHOLE declared set rather than only the constraint that failed.
    if row.path == "tagKeys" {
        nested = format!(
            "Member must have length less than or equal to 128, Member must have length \
             greater than or equal to 1, {nested}"
        );
    }
    if row.carried {
        nested = format!("{nested}, {ENTRY_PRESENT}");
    }
    format!(
        "1 validation error detected: Value '[{}]' at '{}' failed to satisfy constraint: \
         Member must satisfy constraint: [{nested}]",
        row.quoted, row.path
    )
}

/// RULE: the nested set closes with the presence requirement exactly where the model never
/// leaves the list member optional.
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
            row.carried,
            "case {:?}: expected the presence requirement nested = {}, got {message:?}",
            row.case,
            row.carried
        );
    }
}
