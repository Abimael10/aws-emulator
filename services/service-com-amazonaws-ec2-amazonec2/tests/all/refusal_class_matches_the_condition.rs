// SPDX-License-Identifier: CC0-1.0

//! WHICH error code a refusal carries, for the conditions where the class of the fault — not
//! just the fact of it — is what a client branches on.
//!
//! Every call goes over the wire the service actually speaks, through the shared
//! `super::ec2_query` harness: an `application/x-www-form-urlencoded` ec2Query POST dispatched
//! through the same `MultiServiceRouter` the emulator serves with, carrying the Region in the
//! SigV4 `Credential=` scope.
//!
//! EC2 declares no error SHAPES at all (`smithy_query(query="operation", …)` reports
//! `errors: []` for every operation of this model), so the `<Code>` of the ec2Query error
//! envelope is the whole contract a client can branch on, and the rules below are about that
//! code. Each is asserted over a table whose rows differ along what the rule generalises over
//! — the value, the member, the bound, the operation that shares the refusal path — rather
//! than around any one call, and each table carries a row the rule does NOT refuse, so no
//! assertion can pass by refusing everything.
//!
//! The rules under test:
//!
//! 1. A value outside the catalog a modelled enum publishes is a parameter carrying a value
//!    the service cannot use: `InvalidParameterValue`, in the ONE sentence this crate words a
//!    catalog refusal with, whichever side of dispatch refused it.
//! 2. A CIDR block of the OTHER IP version than the prefix list's declared address family is a
//!    conflict between two parameters of one request: `InvalidParameterCombination`, while text
//!    that is no CIDR block at all stays `InvalidParameterValue`.
//! 3. A placement group member the model documents as valid only for one `Strategy`, carried
//!    with another strategy, is a value that operation cannot use: `InvalidParameterValue`.
//! 4. A `PolicyDocument` EC2 will not take as a VPC endpoint policy — past the published quota,
//!    or not JSON at all — is `InvalidPolicyDocument` with the one fixed sentence, which states
//!    no clause and publishes no limit.
//! 5. A page size below the floor an operation publishes carries the code THAT operation
//!    answers: EC2 is not uniform here, so two paged operations are refused one page size with
//!    two codes.
//! 6. Public key material that is no key EC2 can import is `InvalidKey.Format`.
//! 7. A prefix list id that resolves to nothing carries the code of the surface that was asked:
//!    the legacy read of a Region's AWS service prefix lists spells it `Id`, the managed prefix
//!    list surface `ID`.
//! 8. A lock period outside the range the snapshot-lock surface publishes is `InvalidRequest`.
//! 9. A VPC attribute modify that names no attribute at all states no modification:
//!    `InvalidParameterCombination`.

use ::base64::Engine as _;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message, indexed};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// One ec2Query form field.
type Param = (String, String);

/// One form field, as the harness takes them.
fn param(name: &str, value: &str) -> Param {
    (name.to_string(), value.to_string())
}

/// One call, as its HTTP status and body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One call required to succeed, as its body.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One call required to be REFUSED with HTTP 400, as the `(code, message)` of its error
/// envelope.
async fn refused(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    params: &[Param],
) -> (String, String) {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 from {action}, got {status} with body {body}"
    );
    (error_code(&body), error_message(&body))
}

/// One element of a body, or a panic naming the case.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// Assert that `action` with `params` is refused `(code, message)`.
async fn assert_refusal(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    params: &[Param],
    expected: (&str, &str),
) {
    let (code, message) = refused(case, router, action, params).await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        expected,
        "case {case:?}: expected {expected:?}, got {:?}",
        (code.as_str(), message.as_str())
    );
}

/// The form fields of a Capacity Reservation Fleet create that would be served, with
/// `criteria` as its `InstanceMatchCriteria` when one is given.
///
/// Everything else is a request the handler accepts — a target capacity, one instance type
/// specification naming a type the catalog offers, a platform out of the modelled catalog and
/// a zone of the signed Region — so the criteria is the only thing a row varies.
fn fleet_params(criteria: Option<&str>) -> Vec<Param> {
    let mut params = vec![
        param("TotalTargetCapacity", "2"),
        param("InstanceTypeSpecification.1.InstanceType", "m5.large"),
        param(
            "InstanceTypeSpecification.1.InstancePlatform",
            "Linux/UNIX",
        ),
        param("InstanceTypeSpecification.1.AvailabilityZone", "us-east-1a"),
        param("InstanceTypeSpecification.1.Weight", "1.0"),
    ];
    if let Some(criteria) = criteria {
        params.push(param("InstanceMatchCriteria", criteria));
    }
    params
}

/// Rule 1: "Currently, Capacity Reservation Fleets support `open` instance matching criteria
/// only" (`CreateCapacityReservationFleetRequest$InstanceMatchCriteria`), so every other word
/// is a value the service cannot use for that parameter — `InvalidParameterValue` with the
/// sentence EC2 words for that member — while the one word the catalog publishes creates a
/// Fleet.
///
/// The rows differ along the VALUE: the criteria another Capacity Reservation surface does
/// publish, the published word in the wrong case, and a word no EC2 surface publishes at all.
/// The expected sentence comes from the crate's own wording helper and the GENERATED catalog,
/// so the table writes out neither. The two closing rows separate the two classes: the
/// published word is served, while a request that carries no criteria at all is refused for
/// the PARAMETER it left out rather than for a value.
#[tokio::test]
async fn a_fleet_criteria_outside_the_published_catalog_is_a_parameter_value_refusal() {
    let (router, _engine) = super::ec2_query::emulator();
    let catalog = ec2::ops::types::FLEET_INSTANCE_MATCH_CRITERIA_VALUES;

    for criteria in ["targeted", "Open", "OPEN", "closed"] {
        let case = format!("a Fleet asked for {criteria:?} matching criteria");
        let expected = ec2::state::fleet_instance_match_criteria_refusal_message(criteria);
        assert_refusal(
            &case,
            &router,
            "CreateCapacityReservationFleet",
            &fleet_params(Some(criteria)),
            (ec2::state::INVALID_PARAMETER_VALUE, expected.as_str()),
        )
        .await;
    }

    // The one word the catalog publishes is served, so the rows above are refused for the
    // value they carry and not for the member; a request that names NO criteria is refused for
    // the parameter it left out instead, which is the other class and the other rule
    // (`refusal_class_before_resource_lookup`).
    let case = format!("a Fleet asked for {:?}", Some(catalog[0]));
    let body = ok(
        &case,
        &router,
        "CreateCapacityReservationFleet",
        &fleet_params(Some(catalog[0])),
    )
    .await;
    assert_eq!(
        element(&body, "instanceMatchCriteria").as_deref(),
        Some(catalog[0]),
        "case {case:?}: expected the Fleet to report the published criteria, got {body}"
    );
    assert_refusal(
        "a Fleet asked for no criteria at all",
        &router,
        "CreateCapacityReservationFleet",
        &fleet_params(None),
        (
            ec2::state::MISSING_PARAMETER,
            "The following parameter is missing: instanceMatchCriteria",
        ),
    )
    .await;
}

/// The form fields of a managed prefix list create of family `family` carrying the one entry
/// `cidr`, under the name `name`.
fn prefix_list_params(name: &str, family: &str, cidr: Option<&str>) -> Vec<Param> {
    let mut params = vec![
        param("PrefixListName", name),
        param("AddressFamily", family),
        param("MaxEntries", "4"),
    ];
    if let Some(cidr) = cidr {
        params.push(param("Entry.1.Cidr", cidr));
    }
    params
}

/// Rule 2: an entry's `Cidr` is read against the list's own `AddressFamily`, and the two ways
/// it can fail are two CLASSES — a block of the other IP version is a conflict between two
/// parameters that are each valid alone (`InvalidParameterCombination`), while text that is no
/// CIDR block at all is one parameter carrying a value the service cannot read
/// (`InvalidParameterValue`).
///
/// The rows differ along the family, the version of the block and the way the text fails, and
/// cover BOTH operations that read an entry through the one reader — the create that names a
/// list's first entries and the modify that adds one — so the rule is pinned where it lives
/// rather than for one call. Two rows the rule serves close the table.
#[tokio::test]
async fn a_prefix_list_entry_of_the_other_family_is_a_parameter_combination_refusal() {
    let (router, _engine) = super::ec2_query::emulator();
    let ipv4 = ec2::state::PREFIX_LIST_ADDRESS_FAMILY_IPV4;
    let ipv6 = ec2::state::PREFIX_LIST_ADDRESS_FAMILY_IPV6;

    // (the family the list declares, the block an entry carries, the expected code)
    let rows: [(&str, &str, &str); 6] = [
        // A well-formed block of the other version: the family and the block disagree.
        (ipv4, "2001:db8::/32", ec2::state::INVALID_PARAMETER_COMBINATION),
        (ipv4, "::/0", ec2::state::INVALID_PARAMETER_COMBINATION),
        (ipv6, "10.0.0.0/8", ec2::state::INVALID_PARAMETER_COMBINATION),
        (ipv6, "0.0.0.0/0", ec2::state::INVALID_PARAMETER_COMBINATION),
        // Text that is no CIDR block of EITHER version: one parameter's own value.
        (ipv4, "not-a-cidr", ec2::state::INVALID_PARAMETER_VALUE),
        (ipv6, "2001:db8::/zz", ec2::state::INVALID_PARAMETER_VALUE),
    ];

    for (index, (family, cidr, expected_code)) in rows.into_iter().enumerate() {
        let case = format!("a {family} list carrying {cidr:?}");
        // Two classes, two sentences: the conflict names the two parameters that cannot be
        // combined, while an unreadable value names the member and the value.
        let expected_message = if expected_code == ec2::state::INVALID_PARAMETER_COMBINATION {
            ec2::ops::modify_managed_prefix_list::managed_prefix_list_entries::
                entry_block_family_mismatch_message(cidr, family)
        } else {
            ec2::ops::modify_managed_prefix_list::managed_prefix_list_entries::
                unreadable_entry_block_message(cidr, family)
        };
        assert_refusal(
            &case,
            &router,
            "CreateManagedPrefixList",
            &prefix_list_params(&format!("entry-row-{index}"), family, Some(cidr)),
            (expected_code, expected_message.as_str()),
        )
        .await;
    }

    // The same reader, reached from the operation that ADDS an entry to a list that already
    // exists: the class is the entry's, not the create's.
    let created = ok(
        "the list a modify adds to",
        &router,
        "CreateManagedPrefixList",
        &prefix_list_params("modify-target", ipv4, Some("10.0.0.0/8")),
    )
    .await;
    let prefix_list_id = member("the list a modify adds to", &created, "prefixListId");
    await_revisable(&router, "the list a modify adds to", &prefix_list_id).await;
    assert_refusal(
        "an IPv6 block added to an IPv4 list",
        &router,
        "ModifyManagedPrefixList",
        &[
            param("PrefixListId", &prefix_list_id),
            param("AddEntry.1.Cidr", "2001:db8::/32"),
        ],
        (
            ec2::state::INVALID_PARAMETER_COMBINATION,
            "An (IPv4) prefix list cannot contain an (2001:db8::/32) CIDR.",
        ),
    )
    .await;

    // A block of the family the list declares is served, in both families: the refusals above
    // are about the disagreement and not about carrying an entry at all.
    for (family, cidr) in [(ipv4, "10.1.0.0/16"), (ipv6, "2001:db8:1::/48")] {
        let case = format!("a {family} list carrying its own {cidr:?}");
        let body = ok(
            &case,
            &router,
            "CreateManagedPrefixList",
            &prefix_list_params(&format!("served-{family}"), family, Some(cidr)),
        )
        .await;
        assert_eq!(
            element(&body, "addressFamily").as_deref(),
            Some(family),
            "case {case:?}: expected the list to report the family it was created with, got \
             {body}"
        );
    }
}

/// Look again until the prefix list `prefix_list_id` has left the state its creation is still
/// in, which is the state a revision of its entries is admitted from.
///
/// A wait on the CONDITION rather than on a span — the settling is the CLOCK's — so a loaded
/// machine polls more often instead of reading the list too early. The deadline is generous,
/// because all it bounds is a hang.
async fn await_revisable(router: &MultiServiceRouter, case: &str, prefix_list_id: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let body = ok(
            case,
            router,
            "DescribeManagedPrefixLists",
            &indexed("PrefixListId", &[prefix_list_id]),
        )
        .await;
        let state = member(case, &body, "state");
        if state != ec2::state::PREFIX_LIST_STATE_CREATE_IN_PROGRESS {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: gave up waiting for {prefix_list_id} to leave {state:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

/// Rule 3: each of `CreatePlacementGroup`'s strategy-scoped members — the partition count, the
/// spread level and the parent group — is "valid only when Strategy is set to" one strategy, so
/// carrying it with any other strategy is a value that operation cannot use:
/// `InvalidParameterValue`, naming the member, the value and the strategy it holds for.
///
/// The rows differ along the MEMBER and along the strategy the request asked for, covering
/// every (member, other strategy) pair the three published strategies admit — so the rule is
/// asserted over the class rather than over the one member a recording happened to carry. The
/// two rows where the member DOES belong to the strategy create a group.
#[tokio::test]
async fn a_placement_group_member_outside_its_strategy_is_a_parameter_value_refusal() {
    let (router, _engine) = super::ec2_query::emulator();

    // (the member as the wire spells it, the value, the strategy it is valid for)
    let scoped: [(&str, &str, &str); 3] = [
        ("partitionCount", "3", "partition"),
        ("spreadLevel", "rack", "spread"),
        ("parentGroupId", "pg-00000000000000001", "cluster"),
    ];
    // The member as the REQUEST names it, beside the wire spelling the refusal uses.
    let requested_name = |member: &str| match member {
        "partitionCount" => "PartitionCount",
        "spreadLevel" => "SpreadLevel",
        _ => "ParentGroupId",
    };

    for (member_name, value, valid_for) in scoped {
        for strategy in ["cluster", "partition", "spread"] {
            if strategy == valid_for {
                continue;
            }
            let case = format!("{member_name} on a {strategy} group");
            let expected = format!(
                "Value ({value}) for parameter {member_name} is invalid. \
                 {declared} is valid only when Strategy is set to {valid_for}.",
                declared = ec2::state::declared_member_name(member_name),
            );
            assert_refusal(
                &case,
                &router,
                "CreatePlacementGroup",
                &[
                    param("GroupName", &format!("{member_name}-on-{strategy}")),
                    param("Strategy", strategy),
                    param(requested_name(member_name), value),
                ],
                (ec2::state::INVALID_PARAMETER_VALUE, expected.as_str()),
            )
            .await;
        }
    }

    // The member carried WITH the strategy it belongs to: served, so the rows above are
    // refused for the pairing and not for the member.
    for (member_name, value, valid_for) in scoped.into_iter().take(2) {
        let case = format!("{member_name} on a {valid_for} group");
        let body = ok(
            &case,
            &router,
            "CreatePlacementGroup",
            &[
                param("GroupName", &format!("{member_name}-served")),
                param("Strategy", valid_for),
                param(requested_name(member_name), value),
            ],
        )
        .await;
        assert_eq!(
            element(&body, "strategy").as_deref(),
            Some(valid_for),
            "case {case:?}: expected the group to report the strategy it asked for, got {body}"
        );
    }
}

/// Rule 4: a `PolicyDocument` EC2 will not take as a VPC endpoint policy is refused with its
/// own code for that member and ONE fixed sentence — which names neither the clause the
/// document failed nor the quota it is held to.
///
/// The rows differ along the WAY the document fails (one character past the published quota,
/// far past it, text that is no JSON at all, JSON that stops halfway) and cover both
/// operations that read the member through the one reader: the create that attaches a document
/// and the modify that replaces one. A document just inside the quota is attached.
#[tokio::test]
async fn a_vpc_endpoint_policy_the_service_will_not_take_is_an_invalid_policy_document() {
    let (router, _engine) = super::ec2_query::emulator();
    const CASE: &str = "the endpoint policy table";
    const SERVICE: &str = "com.amazonaws.us-east-1.s3";
    let vpc = ok(
        CASE,
        &router,
        "CreateVpc",
        &[param("CidrBlock", "10.70.0.0/16")],
    )
    .await;
    let vpc_id = member(CASE, &vpc, "vpcId");

    // Valid JSON of exactly `chars` characters, so a row's SIZE is the only thing wrong with
    // it. The bound comes from the crate's own constant rather than a number written here.
    let policy_of = |chars: usize| -> String {
        let (opening, closing) = (r#"{"Statement":""#, r#""}"#);
        let padding = "x".repeat(chars - opening.len() - closing.len());
        format!("{opening}{padding}{closing}")
    };
    let quota = ec2::state::MAX_VPC_ENDPOINT_POLICY_CHARS;
    let refused_documents: [(&str, String); 4] = [
        ("one character past the quota", policy_of(quota + 1)),
        ("far past the quota", policy_of(quota * 2)),
        ("text that is no JSON", "allow everything".to_string()),
        ("JSON that stops halfway", r#"{"Statement":[{"Effect":"#.to_string()),
    ];

    let expected = (
        ec2::state::INVALID_POLICY_DOCUMENT,
        ec2::state::INVALID_VPC_ENDPOINT_POLICY_MESSAGE,
    );
    for (case, document) in &refused_documents {
        assert_refusal(
            case,
            &router,
            "CreateVpcEndpoint",
            &[
                param("VpcId", &vpc_id),
                param("ServiceName", SERVICE),
                param("VpcEndpointType", "Gateway"),
                param("PolicyDocument", document),
            ],
            expected,
        )
        .await;
    }

    // A document just inside the quota is attached, which is also the endpoint the modify
    // rows below are refused on.
    let inside = policy_of(quota);
    let created = ok(
        "a document at the quota",
        &router,
        "CreateVpcEndpoint",
        &[
            param("VpcId", &vpc_id),
            param("ServiceName", SERVICE),
            param("VpcEndpointType", "Gateway"),
            param("PolicyDocument", &inside),
        ],
    )
    .await;
    let endpoint_id = member("a document at the quota", &created, "vpcEndpointId");
    assert_eq!(
        super::ec2_query::unescaped(&member(
            "a document at the quota",
            &created,
            "policyDocument"
        ))
        .chars()
        .count(),
        quota,
        "case \"a document at the quota\": expected the document the request sent, got {created}"
    );

    for (case, document) in &refused_documents {
        assert_refusal(
            &format!("{case}, on a modify"),
            &router,
            "ModifyVpcEndpoint",
            &[
                param("VpcEndpointId", &endpoint_id),
                param("PolicyDocument", document),
            ],
            expected,
        )
        .await;
    }
}

/// Rule 5: a page size below the floor an operation publishes carries the code THAT operation
/// answers — EC2 is not uniform about it — and the floor itself comes from the member's own
/// `@range`.
///
/// The rows differ along the VALUE below the floor (one short of it, one, none, a negative
/// count) and along the OPERATION: the prefix list association report, whose floor refusal is
/// EC2's parameter-value code, against the managed prefix list describe, whose floor refusal is
/// EC2's own page-size code. A page size at each published bound is served.
#[tokio::test]
async fn a_page_size_below_the_floor_carries_the_code_the_operation_answers() {
    let (router, _engine) = super::ec2_query::emulator();
    const CASE: &str = "the page size table";
    let created = ok(
        CASE,
        &router,
        "CreateManagedPrefixList",
        &prefix_list_params(
            "page-size-source",
            ec2::state::PREFIX_LIST_ADDRESS_FAMILY_IPV4,
            Some("10.0.0.0/8"),
        ),
    )
    .await;
    let prefix_list_id = member(CASE, &created, "prefixListId");

    // `GetManagedPrefixListAssociationsMaxResults` publishes `@range` 5..255.
    for value in ["4", "1", "0", "-3"] {
        let case = format!("an association page of {value}");
        let expected = ec2::state::page_size_outside_range_message(
            // The input shape being read chooses the wording; the floor comes from the
            // member's own `@range`, as the constraint walk reports it.
            Some("GetManagedPrefixListAssociationsRequest"),
            value,
            "5",
            ec2::state::PageSizeBound::Smallest,
        );
        assert_refusal(
            &case,
            &router,
            "GetManagedPrefixListAssociations",
            &[
                param("PrefixListId", &prefix_list_id),
                param("MaxResults", value),
            ],
            (ec2::state::INVALID_PARAMETER_VALUE, expected.as_str()),
        )
        .await;
    }
    // The same condition on the operation beside it: `PrefixListMaxResults` publishes 1..100,
    // and ITS floor refusal is EC2's own page-size code with that surface's own sentence.
    let (code, _message) = refused(
        "a describe page of 0",
        &router,
        "DescribeManagedPrefixLists",
        &[param("MaxResults", "0")],
    )
    .await;
    assert_eq!(
        code,
        ec2::state::INVALID_MAX_RESULTS,
        "case \"a describe page of 0\": expected the describe's own page-size code, got {code}"
    );

    // Both published bounds of the association report are served, so the rows above are
    // refused for falling outside the range and not for naming a page size at all.
    for value in ["5", "255"] {
        let case = format!("an association page of {value}");
        ok(
            &case,
            &router,
            "GetManagedPrefixListAssociations",
            &[
                param("PrefixListId", &prefix_list_id),
                param("MaxResults", value),
            ],
        )
        .await;
    }
}

/// Rule 6: material an import cannot read as an RSA or ED25519 public key is refused with the
/// code that names the KEY's format, in the one sentence EC2 words for it.
///
/// The rows differ along the WAY the material fails: text that is no key at all, an OpenSSH
/// line whose blob is not base64, and a key of an algorithm an import does not take.
#[tokio::test]
async fn public_key_material_that_is_no_key_is_an_invalid_key_format() {
    let (router, _engine) = super::ec2_query::emulator();

    let rows: [(&str, &str); 3] = [
        ("text that is no key at all", "this is not a public key"),
        ("a line whose blob is not base64", "ssh-rsa !!!! comment"),
        ("a key of an algorithm an import does not take", "ssh-dss AAAAB3NzaC1kc3M="),
    ];
    for (index, (case, material)) in rows.into_iter().enumerate() {
        assert_refusal(
            case,
            &router,
            "ImportKeyPair",
            &[
                param("KeyName", &format!("import-{index}")),
                // `PublicKeyMaterial` is a blob, so it travels base64-encoded: what each row
                // varies is the MATERIAL, not its transport encoding.
                param(
                    "PublicKeyMaterial",
                    &::base64::engine::general_purpose::STANDARD.encode(material),
                ),
            ],
            (
                ec2::state::INVALID_KEY_FORMAT,
                ec2::state::UNREADABLE_PUBLIC_KEY_MESSAGE,
            ),
        )
        .await;
    }
    assert_eq!(
        ec2::state::INVALID_KEY_FORMAT, "InvalidKey.Format",
        "the import's refusal names the key's own error family"
    );
}

/// Rule 7: a prefix list id that resolves to nothing is refused with the not-found code of the
/// SURFACE that was asked — the legacy read of a Region's AWS service prefix lists spells the
/// id part `Id`, the managed prefix list surface spells it `ID` — under the one sentence both
/// answer.
///
/// The rows differ along the id FORM (EC2's two published id lengths) and along the operation
/// asked, so neither spelling can be answered by the other surface. A request naming no id at
/// all reports the Region's published lists.
#[tokio::test]
async fn a_service_prefix_list_id_that_resolves_to_nothing_carries_the_service_reads_code() {
    let (router, _engine) = super::ec2_query::emulator();

    for unknown in ["pl-0f1e2d3c", "pl-0123456789abcdef0"] {
        // Both surfaces word the class the same way — the sentence every operation that
        // resolves a prefix list to report on it answers — so the CODE is the only thing that
        // differs between them.
        let expected = ec2::state::prefix_list_does_not_exist_message(unknown);
        assert_refusal(
            &format!("the service read asked about {unknown}"),
            &router,
            "DescribePrefixLists",
            &indexed("PrefixListId", &[unknown]),
            (
                ec2::state::INVALID_PREFIX_LIST_ID_NOT_FOUND_SERVICE_READ,
                expected.as_str(),
            ),
        )
        .await;
        assert_refusal(
            &format!("the managed read asked about {unknown}"),
            &router,
            "DescribeManagedPrefixLists",
            &indexed("PrefixListId", &[unknown]),
            (
                ec2::state::INVALID_PREFIX_LIST_ID_NOT_FOUND,
                expected.as_str(),
            ),
        )
        .await;
    }
    assert_eq!(
        ec2::state::INVALID_PREFIX_LIST_ID_NOT_FOUND_SERVICE_READ,
        "InvalidPrefixListId.NotFound",
        "the service read's not-found code spells the id part Id"
    );
    assert_ne!(
        ec2::state::INVALID_PREFIX_LIST_ID_NOT_FOUND_SERVICE_READ,
        ec2::state::INVALID_PREFIX_LIST_ID_NOT_FOUND,
        "the two surfaces answer two codes"
    );

    // A read that narrows to nothing still reports the Region's published lists, so the
    // refusals above are about the id and not about the operation.
    let body = ok("the whole published report", &router, "DescribePrefixLists", &[]).await;
    assert!(
        element(&body, "prefixListId").is_some(),
        "case \"the whole published report\": expected the Region's published lists, got {body}"
    );
}

/// Rule 8: the snapshot-lock surface refuses a lock period outside the range the model
/// publishes with EC2's `InvalidRequest`, naming the member, the value and the bound.
///
/// The rows differ along the MEMBER (the lock's duration and the compliance mode's cooling-off
/// period), along the BOUND missed (below the floor, above the ceiling) and along how far
/// outside the value is. A value at each published bound reaches the handler instead.
#[tokio::test]
async fn a_lock_period_outside_the_published_range_is_an_invalid_request() {
    let (router, _engine) = super::ec2_query::emulator();
    /// A snapshot id of the form EC2 issues: the request is refused for its period before
    /// anything resolves the snapshot.
    const SNAPSHOT: &str = "snap-0123456789abcdef0";

    // (the member as the model declares it, the member as the wire spells it, the mode it
    // belongs to, the value, the bound it missed, the comparison AWS's clause states)
    let rows: [(&str, &str, &str, &str, &str, &str); 5] = [
        // `RetentionPeriodRequestDays` publishes `@range` 1..36500.
        ("LockDuration", "lockDuration", "governance", "36501", "36500", "less"),
        ("LockDuration", "lockDuration", "governance", "100000", "36500", "less"),
        ("LockDuration", "lockDuration", "governance", "0", "1", "greater"),
        // `CoolOffPeriodRequestHours` publishes `@range` 1..72.
        ("CoolOffPeriod", "coolOffPeriod", "compliance", "73", "72", "less"),
        ("CoolOffPeriod", "coolOffPeriod", "compliance", "0", "1", "greater"),
    ];
    for (member_name, wire_name, mode, value, bound, comparison) in rows {
        let case = format!("a lock asked for {member_name} {value}");
        // The code is this surface's; the clause is AWS's own account of the value against the
        // bound the member's `@range` publishes, which names the member as the wire spells it.
        let expected = format!(
            "1 validation error detected: Value '{value}' at '{wire_name}' failed to satisfy \
             constraint: Member must have value {comparison} than or equal to {bound}"
        );
        let mut params = vec![
            param("SnapshotId", SNAPSHOT),
            param("LockMode", mode),
            param(member_name, value),
        ];
        if member_name == "CoolOffPeriod" {
            params.push(param("LockDuration", "7"));
        }
        assert_refusal(
            &case,
            &router,
            "LockSnapshot",
            &params,
            (ec2::state::INVALID_REQUEST, expected.as_str()),
        )
        .await;
    }

    // A period at each published bound reaches the HANDLER rather than this refusal: the lock
    // this surface would create is still a scaffold stub, so what it answers today is that 501
    // — asserted exactly, so the row fails if the range mapping ever over-refuses one of the
    // values it must admit. When the stub is implemented
    // (`issues/…-ec2-locksnapshot-stub.md`) these two rows become the lock it then creates.
    for value in ["1", "36500"] {
        let case = format!("a lock asked for LockDuration {value}");
        let (status, body) = call(
            &router,
            "LockSnapshot",
            &[
                param("SnapshotId", SNAPSHOT),
                param("LockMode", "governance"),
                param("LockDuration", value),
            ],
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (501, "NotImplemented"),
            "case {case:?}: a period inside the published range must reach the handler rather \
             than any refusal of its range, got {status} with {body}"
        );
    }
}

/// Rule 9: a `ModifyVpcAttribute` that names the VPC and no attribute at all states no
/// modification — a combination of parameters the service cannot act on
/// (`InvalidParameterCombination`), not an absent parameter, since any ONE of the three
/// attributes would have served.
///
/// The rows differ along WHAT ELSE the request carries: nothing but the VPC, a VPC the account
/// does not hold (so the class is the request's and is decided before anything is resolved),
/// and an attribute structure carrying no value — which IS a parameter the request should have
/// filled in. A request naming one attribute is served.
#[tokio::test]
async fn a_vpc_attribute_modify_that_names_no_attribute_is_a_parameter_combination_refusal() {
    let (router, _engine) = super::ec2_query::emulator();
    const CASE: &str = "the VPC attribute table";
    let created = ok(
        CASE,
        &router,
        "CreateVpc",
        &[param("CidrBlock", "10.71.0.0/16")],
    )
    .await;
    let vpc_id = member(CASE, &created, "vpcId");
    /// A VPC id of the right shape that no VPC of the account has.
    const ABSENT_VPC: &str = "vpc-0000000000000beef";

    for named in [vpc_id.as_str(), ABSENT_VPC] {
        assert_refusal(
            &format!("no attribute named at all, on {named}"),
            &router,
            "ModifyVpcAttribute",
            &[param("VpcId", named)],
            (
                ec2::state::INVALID_PARAMETER_COMBINATION,
                ec2::ops::modify_vpc_attribute::NO_ATTRIBUTES_SPECIFIED,
            ),
        )
        .await;
    }

    // An attribute NAMED without a value is the other fault, and keeps its own class: there
    // the request did leave a parameter out.
    let (code, _message) = refused(
        "an attribute named with no value",
        &router,
        "ModifyVpcAttribute",
        &[param("VpcId", &vpc_id), param("EnableDnsSupport", "")],
    )
    .await;
    assert_eq!(
        code,
        ec2::state::MISSING_PARAMETER,
        "case \"an attribute named with no value\": expected the absent-parameter code, got \
         {code}"
    );

    // One attribute named is served, so the refusals above are about naming none.
    let _ = ok(
        "one attribute named",
        &router,
        "ModifyVpcAttribute",
        &[
            param("VpcId", &vpc_id),
            param("EnableDnsHostnames.Value", "true"),
        ],
    )
    .await;
}
