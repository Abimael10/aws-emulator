// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the resource `CreateDomainNameAccessAssociation` is created FOR: the
//! private custom domain name its `domainNameArn` names.
//!
//! The RULE under test, one level above the recorded call:
//!
//! > `CreateDomainNameAccessAssociation` creates "a domain name access association resource
//! > between an access association source and a private custom domain name", so the DOMAIN
//! > NAME is the resource the association is for: it is resolved against this account's own
//! > records BEFORE the access association source is resolved against the service that owns
//! > VPC endpoints, and a `domainNameArn` this service can read as the ARN of a PRIVATE custom
//! > domain name but holds no record for is the modelled `BadRequestException` (400) carrying
//! > the fixed sentence `Domain not found` — whatever the source the same request named says,
//! > and with nothing stored.
//!
//! Two things follow from that one rule and both are asserted: WHICH sentence the refusal
//! carries, and WHICH of the two resources a request naming neither is refused for. The rows
//! span what the rule generalises over: the form of the private domain name ARN that answers
//! to no record (a short name, a deeply nested name, a name in another Region, the name of a
//! domain name that was created and then DELETED), crossed with the form of the
//! source the same request carries (an id the account has no endpoint under, an id of an
//! endpoint it DOES have, and a value that is not an endpoint id at all) — so no row can pass
//! by the source happening to be refused first.
//!
//! Amazon EC2 is mounted beside this service on one engine, so the endpoint a row names is
//! created through EC2's own API: without it the source of a well-formed id could never
//! resolve, and the "the source is fine, the domain name is not" rows would prove nothing.
//!
//! [`Gateway`], [`gateway_with_ec2`], [`page_items`] and [`string_member`] are this crate's
//! one wire-test harness; the create, list and private-domain helpers are the access
//! association suite's own, and the EC2 and domain-name helpers the sibling suites'.

use serde_json::json;
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    DOMAIN_NAME_NOT_FOUND, INVALID_VPC_ENDPOINT_ID,
};

use crate::domain_name_access_association_endpoint::{
    execute_api_endpoint, network, not_a_valid_endpoint_id,
};
use crate::domain_name_access_associations::{
    association_body, create_association, list_associations, private_domain,
};
use crate::rest_api_lifecycle::{gateway_with_ec2, page_items, string_member, Gateway};
use crate::tag_reads_by_arn::apigateway_arn;

const BAD_REQUEST: &str = "BadRequestException";

/// The sentence a well-formed `domainNameArn` naming no stored record earns, written out
/// HERE as a literal — with the service's own constant asserted to
/// equal it below, so a change of wording is caught rather than followed: an assertion made
/// only against the constant would compare the implementation with itself. ENTIRELY FIXED
/// text, as the sibling `fixed_refusal_sentences` suite keeps the other fixed sentences.
///
/// Source: API Gateway's recorded refusal of a create naming no such domain name.
const DOMAIN_NOT_FOUND_SENTENCE: &str = "Domain not found";

/// `DeleteDomainName` for the record the domain name ARN `arn` names, which the sibling
/// delete suite addresses as `/domainnames/{domainName}?domainNameId={domainNameId}` — the
/// pair of identifiers the operation binds, read back out of the ARN's own resource portion
/// (`/domainnames/{domainName}+{domainNameId}` for a private record).
async fn delete_domain_name(gateway: &Gateway, arn: &str, case: &str) {
    let key = arn
        .rsplit_once("/domainnames/")
        .map(|(_, key)| key.to_string())
        .unwrap_or_else(|| panic!("case {case:?}: the ARN carries the domain name key: {arn:?}"));
    let uri = match key.split_once('+') {
        Some((domain_name, domain_name_id)) => {
            format!("/domainnames/{domain_name}?domainNameId={domain_name_id}")
        }
        None => format!("/domainnames/{key}"),
    };
    let answer = gateway.call("DELETE", &uri, None).await;
    assert_eq!(
        answer.status, 202,
        "case {case:?}: expected DeleteDomainName to answer 202, got {} with {:?}",
        answer.status, answer.body
    );
}

#[tokio::test]
async fn the_domain_name_an_association_is_for_is_resolved_first_and_by_its_own_sentence() {
    // The service's own constant is that sentence, so no second wording of this condition can
    // reach a caller.
    assert_eq!(
        DOMAIN_NAME_NOT_FOUND, DOMAIN_NOT_FOUND_SENTENCE,
        "case the service constant: expected {DOMAIN_NOT_FOUND_SENTENCE:?}, got \
         {DOMAIN_NAME_NOT_FOUND:?}"
    );
    let gateway = gateway_with_ec2();
    let network = network(&gateway).await;
    let stored = private_domain(&gateway, "resolved.example.com").await;
    // An endpoint the account really has, so a row can carry a source that resolves.
    let endpoint = execute_api_endpoint(&gateway, &network, "the account's own endpoint").await;

    // A domain name that EXISTS and is then deleted: its ARN stays readable, and the record
    // behind it is gone, which is the same fact as an ARN that never named one.
    let deleted = private_domain(&gateway, "deleted.example.com").await;
    delete_domain_name(&gateway, &deleted, "a domain name that was deleted").await;

    // Rows differ along the FORM of the private domain name ARN that answers to no record.
    let absent: Vec<(String, String)> = vec![
        (
            "a domain name no record answers to".to_string(),
            apigateway_arn("/domainnames/absent.example.com+abcd1234"),
        ),
        (
            "a deeply nested name, for no record".to_string(),
            apigateway_arn("/domainnames/absent.private.api.example.co.uk+zzzzz99999"),
        ),
        (
            "a domain name of another Region".to_string(),
            "arn:aws:apigateway:eu-west-1::/domainnames/absent.example.com+abcd1234".to_string(),
        ),
        (
            "a domain name that was created and then deleted".to_string(),
            deleted,
        ),
    ];
    // ... crossed with the form of the source the same request carries, so the refusal
    // cannot be the source's.
    let sources: Vec<(&str, String)> = vec![
        (
            "an id the account has no endpoint under",
            "vpce-0123456789abcdef0".to_string(),
        ),
        ("an endpoint the account does have", endpoint.clone()),
        (
            "a value that is not an endpoint id at all",
            "not-a-valid-vpce-id".to_string(),
        ),
    ];

    for (domain_case, domain_name_arn) in &absent {
        for (source_case, source) in &sources {
            let case = format!("{domain_case}, with {source_case}");
            let answer =
                create_association(&gateway, association_body(domain_name_arn, source)).await;
            assert_eq!(
                (answer.status, answer.error.as_deref()),
                (400, Some(BAD_REQUEST)),
                "case {case:?}: expected 400 {BAD_REQUEST}, got {} {:?} with {:?}",
                answer.status,
                answer.error,
                answer.body
            );
            let message = string_member(&answer.body, "message", &case);
            assert_eq!(
                message, DOMAIN_NOT_FOUND_SENTENCE,
                "case {case:?}: expected the domain name to be resolved first, with its own \
                 sentence, got {message:?}"
            );
        }
    }

    // The source is still resolved for a domain name that IS there: the order puts the
    // domain name first, it does not drop the second question.
    for (source_case, source) in &sources {
        if source == &endpoint {
            continue;
        }
        let case = format!("a stored private domain name, with {source_case}");
        let answer = create_association(&gateway, association_body(&stored, source)).await;
        assert_eq!(
            (answer.status, answer.error.as_deref()),
            (400, Some(BAD_REQUEST)),
            "case {case:?}: expected 400 {BAD_REQUEST}, got {} {:?} with {:?}",
            answer.status,
            answer.error,
            answer.body
        );
        let message = string_member(&answer.body, "message", &case);
        // Either SOURCE sentence is the source's own, which is what this asserts: the
        // well-formed id the account has no endpoint under earns the one that names it, and
        // the value that is no endpoint id at all the fixed one — and neither is the domain
        // name's.
        let expected = if source.starts_with("vpce-") {
            not_a_valid_endpoint_id(source)
        } else {
            INVALID_VPC_ENDPOINT_ID.to_string()
        };
        assert_eq!(
            message, expected,
            "case {case:?}: expected the SOURCE refusal once the domain name resolved, got \
             {message:?}"
        );
    }

    // The positive control, read back through the implemented reader: the pair whose BOTH
    // resources resolve is created, and it is the only record the refusals above left.
    let created = create_association(&gateway, association_body(&stored, &endpoint)).await;
    assert_eq!(
        created.status, 201,
        "case \"both resources resolve\": expected 201, got {} with {:?}",
        created.status, created.body
    );
    let listed = list_associations(&gateway, "limit=500").await;
    let items = page_items(&listed.body, "after every refusal");
    assert_eq!(
        items.len(),
        1,
        "case \"after every refusal\": expected only the one admitted association, got {:?}",
        listed.body
    );
    assert_eq!(
        string_member(&items[0], "accessAssociationSource", "the admitted association"),
        endpoint,
        "case \"after every refusal\": expected the admitted association's own source, got \
         {:?}",
        items[0]
    );
    assert_eq!(
        string_member(&items[0], "domainNameArn", "the admitted association"),
        stored,
        "case \"after every refusal\": expected the stored domain name's own ARN, got {:?}",
        items[0]
    );
}

#[tokio::test]
async fn a_domain_name_arn_that_cannot_be_read_at_all_is_a_different_refusal() {
    let gateway = gateway_with_ec2();

    // The sentence above is for an ARN this service CAN read and holds no record for. A value
    // it cannot read as a domain name ARN at all is a malformed request rather than a missing
    // record, so it keeps the service's unreadable-ARN sentence: the rows below span the
    // forms of that, and every one of them must NOT answer the missing-record sentence.
    let unreadable = [
        ("a value that is not an arn", "not-a-valid-arn".to_string()),
        ("a bare domain name", "private.example.com".to_string()),
        ("an empty value", String::new()),
        ("an arn of another vendor", "arn:aws:s3:::my-bucket".to_string()),
        (
            "an arn naming a rest api",
            apigateway_arn("/restapis/abcde12345"),
        ),
    ];
    for (case, domain_name_arn) in &unreadable {
        let answer = create_association(
            &gateway,
            json!({
                "domainNameArn": domain_name_arn,
                "accessAssociationSourceType": "VPCE",
                "accessAssociationSource": "vpce-0123456789abcdef0",
            }),
        )
        .await;
        assert_eq!(
            (answer.status, answer.error.as_deref()),
            (400, Some(BAD_REQUEST)),
            "case {case:?}: expected 400 {BAD_REQUEST}, got {} {:?} with {:?}",
            answer.status,
            answer.error,
            answer.body
        );
        let message = string_member(&answer.body, "message", case);
        assert_ne!(
            message, DOMAIN_NOT_FOUND_SENTENCE,
            "case {case:?}: expected the unreadable-ARN refusal rather than the \
             missing-record one, got {message:?}"
        );
    }
}
