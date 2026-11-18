// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `RejectDomainNameAccessAssociation`: how it reads the two ARNs
//! it takes, and what it answers for a pair that names nothing.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The emulator and the request builder are this
//! crate's existing harness, reused from `rest_api_lifecycle`.
//!
//! The RULE under test: each of the two required query members is the ARN of a resource
//! of THIS service of the kind that member names — an access association for
//! `domainNameAccessAssociationArn`, a PRIVATE custom domain name for `domainNameArn`,
//! whose resource portion carries the `domainNameId` "supported only for private custom
//! domain names" — so a value that is not an ARN, an ARN of another vendor, an ARN naming
//! another kind of resource, or a domain name ARN that names no private record is
//! refused as the modelled `BadRequestException` (400), while a well-formed pair that
//! names no stored record is the modelled `NotFoundException` (404). Neither is a 501,
//! and the two answers are not interchangeable: the first tells the caller to fix the
//! request, the second that the resource is not there.
//!
//! The 404 rows are what a well-formed pair naming no stored record resolves to, including
//! the row whose DOMAIN NAME is stored: the handler resolves both records through the same
//! lookup the tag operations resolve an ARN with, so what this operation says an ARN names is
//! what every other reader of it says.

use crate::domain_name_access_associations::private_domain;
use crate::rest_api_lifecycle::{assert_failure, Gateway};
use crate::tag_reads_by_arn::{apigateway_arn, encode_label};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    INVALID_DOMAIN_NAME_ARN, NO_SUCH_ACCESS_ASSOCIATION,
};

/// A well-formed ARN of a domain name access association of this service: the PAIR an
/// association IS, spelled the way this service's access-association collection spells it.
fn association_arn(domain_name_key: &str, access_association_source: &str) -> String {
    apigateway_arn(&format!(
        "/domainnameaccessassociations/domainname/{domain_name_key}/vpcesource/\
         {access_association_source}"
    ))
}

/// A well-formed ARN of a PRIVATE custom domain name of this service: its name and the
/// `domainNameId` that identifies one private record of that name, which is what the resource
/// portion of such an ARN carries.
fn domain_name_arn(domain: &str) -> String {
    apigateway_arn(&format!("/domainnames/{domain}+abcd123456"))
}

/// `RejectDomainNameAccessAssociation` for the pair `(association, domain_name)`, both
/// carried in the query string the way the model binds them.
async fn reject(gateway: &Gateway, association: &str, domain_name: &str) -> crate::rest_api_lifecycle::Answer {
    gateway
        .call(
            "POST",
            &format!(
                "/rejectdomainnameaccessassociations\
                 ?domainNameAccessAssociationArn={}&domainNameArn={}",
                encode_label(association),
                encode_label(domain_name)
            ),
            None,
        )
        .await
}

#[tokio::test]
async fn a_malformed_arn_pair_is_the_modelled_bad_request() {
    // Rows differ along WHICH member carries the unreadable value and HOW it is
    // unreadable: not an ARN at all, an ARN missing fields, an ARN of another vendor, an
    // API Gateway ARN naming another kind of resource, an empty resource portion, and the
    // two members' values swapped — each of which is a request the caller must fix
    // rather than a resource that is not there.
    let good_association = association_arn("api.example.com+abcd123456", "vpce-0a1b2c3d");
    let good_domain = domain_name_arn("api.example.com");
    let rows: Vec<(&str, String, String)> = vec![
        (
            "an association arn that is not an arn",
            "not-a-valid-arn".to_string(),
            good_domain.clone(),
        ),
        (
            "a domain name arn that is not an arn",
            good_association.clone(),
            "api.example.com".to_string(),
        ),
        (
            "a domain name arn that names no private record",
            good_association.clone(),
            apigateway_arn("/domainnames/api.example.com"),
        ),
        (
            "both values unreadable",
            "nope".to_string(),
            "also-nope".to_string(),
        ),
        (
            "an arn with too few fields",
            "arn:aws:apigateway".to_string(),
            good_domain.clone(),
        ),
        (
            "an arn of another vendor",
            "arn:aws:s3:::my-bucket".to_string(),
            good_domain.clone(),
        ),
        (
            "an association arn naming a rest api",
            apigateway_arn("/restapis/abcde12345"),
            good_domain.clone(),
        ),
        (
            "a domain name arn naming an api key",
            good_association.clone(),
            apigateway_arn("/apikeys/abcde12345"),
        ),
        (
            "an empty resource portion",
            apigateway_arn("/"),
            good_domain.clone(),
        ),
        (
            "an association arn with no id",
            apigateway_arn("/domainnameaccessassociations/"),
            good_domain.clone(),
        ),
        (
            "the two members swapped",
            good_domain.clone(),
            good_association.clone(),
        ),
    ];

    let gateway = Gateway::new();
    for (case, association, domain_name) in &rows {
        let answer = reject(&gateway, association, domain_name).await;
        assert_failure(&answer, 400, "BadRequestException", case);
        // ONE sentence for a pair this operation could not read as a private custom domain
        // name and an access association of it, and it carries no part of either value.
        assert_eq!(
            answer.body["message"].as_str(),
            Some(INVALID_DOMAIN_NAME_ARN),
            "case {case}: expected {INVALID_DOMAIN_NAME_ARN:?}, got {:?}",
            answer.body["message"]
        );
    }
}

#[tokio::test]
async fn a_well_formed_pair_naming_no_record_is_the_modelled_not_found() {
    // Rows differ along the pair the two readable ARNs name: nothing about which domain
    // name or source is named changes the answer while no record answers to either.
    let rows: Vec<(&str, String, String)> = vec![
        (
            "an association and a domain name",
            association_arn("api.example.com+abcd123456", "vpce-0a1b2c3d"),
            domain_name_arn("api.example.com"),
        ),
        (
            "another association of the same domain",
            association_arn("api.example.com+abcd123456", "vpce-0123456789abcdef0"),
            domain_name_arn("api.example.com"),
        ),
        (
            "a domain name carrying a trailing label",
            association_arn(
                "private.api.example.co.uk+abcd123456",
                "vpce-0123456789abcdef0",
            ),
            domain_name_arn("private.api.example.co.uk"),
        ),
        (
            "an association whose source is in another case",
            association_arn("api.example.com+abcd123456", "VPCE-0A1B2C3D"),
            domain_name_arn("api.example.com"),
        ),
    ];

    let gateway = Gateway::new();
    for (case, association, domain_name) in &rows {
        let answer = reject(&gateway, association, domain_name).await;
        assert_failure(&answer, 404, "NotFoundException", case);
        // The one sentence API Gateway states for a pair naming no association of the
        // private custom domain name it names.
        assert_eq!(
            answer.body["message"].as_str(),
            Some(NO_SUCH_ACCESS_ASSOCIATION),
            "case {case}: expected {NO_SUCH_ACCESS_ASSOCIATION:?}, got {:?}",
            answer.body["message"]
        );
    }

    // The same answer when the DOMAIN NAME does exist and only the access association does
    // not: the rejection resolves both records, so the one that is missing is the one it is
    // refused for — still the modelled 404, and not a success.
    let stored = private_domain(&gateway, "private.example.com").await;
    let stored_key = stored
        .rsplit("/domainnames/")
        .next()
        .expect("a domain name ARN carries its key")
        .to_string();
    let answer = reject(
        &gateway,
        &association_arn(&stored_key, "vpce-0a1b2c3d"),
        &stored,
    )
    .await;
    assert_failure(
        &answer,
        404,
        "NotFoundException",
        "a stored domain name with no such association",
    );
    assert_eq!(
        answer.body["message"].as_str(),
        Some(NO_SUCH_ACCESS_ASSOCIATION),
        "case a stored domain name with no such association: expected \
         {NO_SUCH_ACCESS_ASSOCIATION:?}, got {:?}",
        answer.body["message"]
    );
}

#[tokio::test]
async fn an_empty_arn_is_read_as_a_malformed_one_rather_than_as_an_absent_record() {
    // Both members are present but carry nothing: an empty value is not an ARN, so it is
    // the same bad request a value of any other unreadable shape earns — and the refusal
    // names this service's own `BadRequestException`, the code AWS names a refused
    // control-plane request by.
    let association = association_arn("api.example.com+abcd123456", "vpce-0a1b2c3d");
    let domain_name = domain_name_arn("api.example.com");
    let rows: Vec<(&str, String, String)> = vec![
        ("an empty association arn", String::new(), domain_name.clone()),
        ("an empty domain name arn", association.clone(), String::new()),
        ("both empty", String::new(), String::new()),
    ];

    let gateway = Gateway::new();
    for (case, association, domain_name) in &rows {
        assert_failure(
            &reject(&gateway, association, domain_name).await,
            400,
            "BadRequestException",
            case,
        );
    }
}
