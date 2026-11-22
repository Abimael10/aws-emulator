// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DeleteDomainNameAccessAssociation`, read back through the
//! operations that report the record it removes (`GetDomainNameAccessAssociations`,
//! `GetTags` by the association's ARN).
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns.
//!
//! The RULES under test, each over a SET of inputs rather than one recorded call:
//!
//! * a delete removes exactly the one stored association its ARN addresses, so the reads
//!   that reported that record stop reporting it while every sibling is untouched, and
//!   nothing the record carries conditions the removal (its tags, the form of its source
//!   identifier, or which private domain name it was created against — including two
//!   private records that share one name, which are two domain names);
//! * removing an association releases the pair it spells: the pair
//!   `(domain name, source)` is free for a new create, and the ARN of a new association of
//!   that pair is the one the delete released;
//! * an ARN this service cannot read as one of ITS access association ARNs is the
//!   modelled `BadRequestException` (400), while a well-formed one no stored record
//!   answers to is the modelled `NotFoundException` (404) rather than a silent success —
//!   which is what makes a second delete of one record, and concurrent deletes of one
//!   record over a table of racer counts, resolve to one success and 404s rather than to a
//!   conflict;
//! * the ARN that addresses one of these records spells the PAIR the association IS, so the
//!   value a create reports is the value every later call resolves — the tag read, the
//!   delete, and the collection that lists it;
//! * a refused delete changes nothing a later caller sees.
//!
//! None of those answers is a 501: every request a caller can make against this operation
//! is one of the statuses its model declares.
//!
//! [`Gateway`], [`Answer`], [`assert_failure`], [`string_member`] and [`page_items`] are
//! this crate's one wire-test harness; `private_domain`, `association_body`,
//! `create_association`, `list_associations` and `listed_arns` are the access-association
//! suite's own calls, and `apigateway_arn` / `encode_label` / `get_tags` the tagging
//! suite's one spelling of an ARN, of a path label and of a tag read.

use std::sync::Arc;

use serde_json::json;
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    INVALID_ACCESS_ASSOCIATION_ARN, NO_SUCH_ACCESS_ASSOCIATION,
};

use crate::domain_name_access_associations::{
    association_body, create_association, list_associations, listed_arns, private_domain,
};
use crate::rest_api_lifecycle::{assert_failure, page_items, string_member, Answer, Gateway};
use crate::tag_reads_by_arn::{apigateway_arn, encode_label, get_tags};

const BAD_REQUEST: &str = "BadRequestException";
const NOT_FOUND: &str = "NotFoundException";

/// The status `DeleteDomainNameAccessAssociation`'s model declares for a success
/// (`smithy_query(query="operation", operation="DeleteDomainNameAccessAssociation")`:
/// `DELETE /domainnameaccessassociations/{domainNameAccessAssociationArn} -> 202`).
const DELETED: u16 = 202;

/// `DeleteDomainNameAccessAssociation` for `arn`, which the model binds as a path label —
/// so the whole ARN travels percent-encoded inside ONE path segment.
async fn delete_association(gateway: &Gateway, arn: &str) -> Answer {
    gateway
        .call(
            "DELETE",
            &format!("/domainnameaccessassociations/{}", encode_label(arn)),
            None,
        )
        .await
}

/// A well-formed ARN of a domain name access association of this service: the PAIR an
/// association IS — the private custom domain name it is for and the source that may invoke
/// it — spelled the way this service's access-association collection spells it (the
/// `domainNameAccessAssociationArn` a create reports is this, for the pair it stored).
fn association_arn(domain_name_key: &str, access_association_source: &str) -> String {
    apigateway_arn(&format!(
        "/domainnameaccessassociations/domainname/{domain_name_key}/vpcesource/\
         {access_association_source}"
    ))
}

/// The `domain_name_key` an API Gateway domain name ARN carries: the name of the domain name
/// and, for a private record, the `domainNameId` that identifies it.
fn domain_name_key_of(domain_name_arn: &str) -> String {
    domain_name_arn
        .rsplit("/domainnames/")
        .next()
        .expect("a domain name ARN carries its key")
        .to_string()
}

/// The `DeleteDomainName` URI that addresses the record `domain_name_arn` names.
///
/// A private record is keyed `<domainName>+<domainNameId>` — several private records may
/// carry one name, so both members address one — and that key is the resource portion of
/// its ARN, which is what this reads it out of.
fn delete_domain_uri(domain_name_arn: &str) -> String {
    let key = domain_name_arn
        .rsplit("/domainnames/")
        .next()
        .expect("a domain name ARN carries its key");
    match key.split_once('+') {
        Some((domain_name, domain_name_id)) => {
            format!("/domainnames/{domain_name}?domainNameId={domain_name_id}")
        }
        None => format!("/domainnames/{key}"),
    }
}

/// Assert a delete was accepted with the status its model declares and an empty body.
fn assert_deleted(answer: &Answer, case: &str) {
    assert_eq!(
        answer.status, DELETED,
        "case {case}: expected {DELETED}, got {} with {:?}",
        answer.status, answer.body
    );
    assert!(
        answer.body.is_null(),
        "case {case}: expected an empty body, got {:?}",
        answer.body
    );
}

/// The ARNs of every stored association, in the order the collection reports them.
async fn stored_arns(gateway: &Gateway, case: &str) -> Vec<String> {
    let answer = list_associations(gateway, "limit=500").await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected GetDomainNameAccessAssociations to answer 200, got {} with {:?}",
        answer.status, answer.body
    );
    listed_arns(&answer.body, case)
}

/// `CreateDomainNameAccessAssociation` for the pair, asserting it was stored and answering
/// the ARN the service assigned it.
async fn stored_association(gateway: &Gateway, domain_name_arn: &str, source: &str) -> String {
    let answer = create_association(gateway, association_body(domain_name_arn, source)).await;
    assert_eq!(
        answer.status, 201,
        "case CreateDomainNameAccessAssociation({source}): expected 201, got {} with {:?}",
        answer.status, answer.body
    );
    string_member(
        &answer.body,
        "domainNameAccessAssociationArn",
        &format!("CreateDomainNameAccessAssociation({source})"),
    )
}

#[tokio::test]
async fn deleting_an_association_removes_that_record_alone_and_a_second_delete_is_not_found() {
    let gateway = Gateway::new();
    // Two private records carrying ONE name are two domain names — a `domainNameId` is
    // "supported only for private custom domain names" and is what identifies one — so an
    // association against each is an association against a different domain name.
    let first = private_domain(&gateway, "private.example.com").await;
    let second = private_domain(&gateway, "private.example.com").await;
    let third = private_domain(&gateway, "other.example.org").await;

    // Rows differ along what the rule generalises over: the domain name the association
    // was created against, the form of its source identifier, and the tag collection it
    // carries — none of which conditions the removal.
    let rows: Vec<(&str, &str, &str, serde_json::Value)> = vec![
        (
            "an untagged association",
            first.as_str(),
            "vpce-0123456789abcdef0",
            json!({}),
        ),
        (
            "a tagged association of the same domain name",
            first.as_str(),
            "vpce-00000000000000001",
            json!({ "Name": "payments" }),
        ),
        (
            "an association of the other private record of one name",
            second.as_str(),
            "vpce-0123456789abcdef0",
            json!({ "cost:center": "team/platform", "empty-value": "" }),
        ),
        (
            "an association of another domain name, the shorter source id width",
            third.as_str(),
            "vpce-0a1b2c3d",
            json!({ "Name": "orders" }),
        ),
    ];

    let mut remaining: Vec<(String, String)> = Vec::new();
    for (case, domain_name_arn, source, tags) in &rows {
        let mut body = association_body(domain_name_arn, source);
        if tags.as_object().is_some_and(|entries| !entries.is_empty()) {
            body["tags"] = tags.clone();
        }
        let answer = create_association(&gateway, body).await;
        assert_eq!(
            answer.status, 201,
            "case {case}: expected 201, got {} with {:?}",
            answer.status, answer.body
        );
        remaining.push((
            (*case).to_string(),
            string_member(&answer.body, "domainNameAccessAssociationArn", case),
        ));
    }

    // Each association is deleted in turn: the delete is accepted, the record stops being
    // listed and stops answering a tag read by its ARN, every sibling is untouched, and a
    // second delete of the same ARN is the modelled not-found.
    while let Some((case, arn)) = remaining.first().cloned() {
        remaining.remove(0);
        assert_deleted(&delete_association(&gateway, &arn).await, &case);

        let again = delete_association(&gateway, &arn).await;
        assert_failure(&again, 404, NOT_FOUND, &format!("a second delete of {case}"));
        assert_eq!(
            again.body["message"].as_str(),
            Some(NO_SUCH_ACCESS_ASSOCIATION),
            "case a second delete of {case}: expected {NO_SUCH_ACCESS_ASSOCIATION:?}, got {:?}",
            again.body["message"]
        );

        let tags = get_tags(&gateway, &arn).await;
        assert_failure(&tags, 404, NOT_FOUND, &format!("GetTags after {case}"));

        let expected: Vec<String> = remaining.iter().map(|(_, arn)| arn.clone()).collect();
        let listed = stored_arns(&gateway, &format!("the collection after {case}")).await;
        assert_eq!(
            listed, expected,
            "case the collection after {case}: expected the records that are left \
             {expected:?}, got {listed:?}"
        );
        for (sibling_case, sibling) in &remaining {
            let sibling_tags = get_tags(&gateway, sibling).await;
            assert_eq!(
                sibling_tags.status, 200,
                "case GetTags({sibling_case}) after {case}: expected the sibling to still \
                 read, got {} with {:?}",
                sibling_tags.status, sibling_tags.body
            );
        }
    }

    let listed = stored_arns(&gateway, "the collection after every delete").await;
    assert!(
        listed.is_empty(),
        "case the collection after every delete: expected an empty page, got {listed:?}"
    );
}

#[tokio::test]
async fn removing_an_association_releases_the_pair_it_spells() {
    let gateway = Gateway::new();
    let domain = private_domain(&gateway, "released.example.com").await;
    // Rows differ along HOW MANY associations the domain name carries when the delete runs,
    // so the release is asserted while another association still stands and once the last
    // one is gone.
    let sources = ["vpce-0123456789abcdef0", "vpce-00000000000000002"];
    let mut arns: Vec<String> = Vec::new();
    for source in sources {
        arns.push(stored_association(&gateway, &domain, source).await);
    }

    for (index, source) in sources.iter().enumerate() {
        let case = format!("the association of {source}");
        assert_deleted(&delete_association(&gateway, &arns[index]).await, &case);

        // The pair is free again: a create of the very same pair is accepted — and it is
        // ADDRESSED the same way, because an access association IS that pair and its ARN
        // spells it, so what the delete released is the pair itself.
        let recreated = stored_association(&gateway, &domain, source).await;
        assert_eq!(
            recreated, arns[index],
            "case {case} recreated: expected the pair's own ARN {:?}, got {recreated:?}",
            arns[index]
        );
        assert_deleted(
            &delete_association(&gateway, &recreated).await,
            &format!("{case} recreated"),
        );
    }

    // The domain name the associations named keeps its own record and is deleted by its own
    // call: this delete removed associations, not domain names.
    let freed = gateway.call("DELETE", &delete_domain_uri(&domain), None).await;
    assert_deleted(&freed, "DeleteDomainName once no association stands");
}

#[tokio::test]
async fn an_arn_this_service_cannot_read_is_the_modelled_bad_request() {
    let gateway = Gateway::new();
    let domain = private_domain(&gateway, "refusals.example.com").await;
    let stored = stored_association(&gateway, &domain, "vpce-0123456789abcdef0").await;

    // Rows differ along HOW the value fails to be one of this service's access association
    // ARNs: not an ARN at all, an ARN missing fields, an ARN of another vendor, an API
    // Gateway ARN naming another kind of resource, an ARN with no resource portion at all,
    // and ARNs of the access-association collection whose portion spells no such pair — no
    // portion, one bare identifier, half the pair, the two halves without the segments that
    // say which half they are, and the halves the wrong way round.
    let stored_source = "vpce-0123456789abcdef0";
    let stored_key = domain_name_key_of(&domain);
    let rows: Vec<(&str, String)> = vec![
        ("a value that is not an arn", "not-a-valid-arn".to_string()),
        ("a bare identifier", "dnaa-1a2b3c4d".to_string()),
        ("an arn with too few fields", "arn:aws:apigateway".to_string()),
        (
            "an arn of another vendor",
            "arn:aws:s3:::my-bucket".to_string(),
        ),
        (
            "an arn naming a rest api",
            apigateway_arn("/restapis/abcde12345"),
        ),
        (
            "an arn naming a domain name",
            apigateway_arn("/domainnames/refusals.example.com"),
        ),
        (
            "an arn naming an api key",
            apigateway_arn("/apikeys/abcde12345"),
        ),
        ("an empty resource portion", apigateway_arn("/")),
        (
            "an arn with no identifier",
            apigateway_arn("/domainnameaccessassociations/"),
        ),
        (
            "an arn carrying one bare identifier",
            apigateway_arn("/domainnameaccessassociations/dnaa1a2b3c"),
        ),
        (
            "an arn carrying the domain name half alone",
            apigateway_arn(&format!(
                "/domainnameaccessassociations/domainname/{stored_key}"
            )),
        ),
        (
            "an arn carrying the source half alone",
            apigateway_arn(&format!(
                "/domainnameaccessassociations/vpcesource/{stored_source}"
            )),
        ),
        (
            "an arn carrying both halves without their segments",
            apigateway_arn(&format!(
                "/domainnameaccessassociations/{stored_key}/{stored_source}"
            )),
        ),
        (
            "an arn carrying the two halves the wrong way round",
            apigateway_arn(&format!(
                "/domainnameaccessassociations/vpcesource/{stored_source}/domainname/\
                 {stored_key}"
            )),
        ),
    ];
    for (case, arn) in &rows {
        let answer = delete_association(&gateway, arn).await;
        assert_failure(&answer, 400, BAD_REQUEST, case);
        // ONE sentence for a value this service could not read as the ARN of one of these
        // records, and it carries none of that value: the same fixed text however the value
        // failed to be one.
        assert_eq!(
            answer.body["message"].as_str(),
            Some(INVALID_ACCESS_ASSOCIATION_ARN),
            "case {case}: expected the unreadable-ARN sentence for this member, got {:?}",
            answer.body["message"]
        );
    }

    // None of the refusals removed anything.
    let listed = stored_arns(&gateway, "the collection after the bad requests").await;
    assert_eq!(
        listed,
        vec![stored.clone()],
        "case the collection after the bad requests: expected {stored:?}, got {listed:?}"
    );
}

#[tokio::test]
async fn a_well_formed_arn_naming_no_stored_association_is_the_modelled_not_found() {
    let gateway = Gateway::new();
    let domain = private_domain(&gateway, "absent.example.com").await;
    let stored = stored_association(&gateway, &domain, "vpce-0123456789abcdef0").await;
    let stored_key = domain_name_key_of(&domain);

    // Rows differ along the PAIR the readable ARN spells: a pair of a domain name no record
    // answers to, the stored domain name with a source it has no association for, the stored
    // pair with its source upper-cased, the stored pair with its source suffixed, and the
    // stored pair with its domain name key one character short. Nothing about which pair is
    // named changes the answer while no record answers to it.
    let rows: Vec<(&str, String)> = vec![
        (
            "a pair of a domain name no record answers to",
            association_arn("nothing.example.com+zzzzz99999", "vpce-0123456789abcdef0"),
        ),
        (
            "the stored domain name with another source",
            association_arn(&stored_key, "vpce-00000000000000009"),
        ),
        (
            "the stored pair with its source upper-cased",
            association_arn(&stored_key, "VPCE-0123456789ABCDEF0"),
        ),
        (
            "the stored pair with its source suffixed",
            association_arn(&stored_key, "vpce-0123456789abcdef0x"),
        ),
        (
            "the stored pair with its domain name key one character short",
            association_arn(
                &stored_key[..stored_key.len() - 1],
                "vpce-0123456789abcdef0",
            ),
        ),
    ];
    for (case, arn) in &rows {
        let answer = delete_association(&gateway, arn).await;
        assert_failure(&answer, 404, NOT_FOUND, case);
        // The same sentence every reader of this ARN answers an unresolvable identifier
        // with: one condition, one message.
        assert_eq!(
            answer.body["message"].as_str(),
            Some(NO_SUCH_ACCESS_ASSOCIATION),
            "case {case}: expected {NO_SUCH_ACCESS_ASSOCIATION:?}, got {:?}",
            answer.body["message"]
        );
    }

    // None of the refusals removed the record that IS stored.
    let listed = stored_arns(&gateway, "the collection after the not-founds").await;
    assert_eq!(
        listed,
        vec![stored.clone()],
        "case the collection after the not-founds: expected {stored:?}, got {listed:?}"
    );
    assert_deleted(
        &delete_association(&gateway, &stored).await,
        "the stored association after the refusals",
    );
}

/// RULE: concurrent deletes of ONE association resolve to exactly one success — the record
/// is looked up and removed under one hold of the store — so every other caller is refused
/// the modelled not-found, whatever the racer count.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_deletes_of_one_association_resolve_to_exactly_one_success() {
    for racers in [2_usize, 3, 6] {
        let case = format!("{racers} concurrent deletes of one association");
        let gateway = Arc::new(Gateway::new());
        let domain = private_domain(&gateway, "race.example.com").await;
        let arn = stored_association(&gateway, &domain, "vpce-0123456789abcdef0").await;
        // A sibling association of the same domain name that no racer addresses: what the
        // contended delete removes is the record its ARN names and nothing else.
        let sibling = stored_association(&gateway, &domain, "vpce-00000000000000002").await;

        let mut tasks = Vec::with_capacity(racers);
        for _ in 0..racers {
            let gateway = Arc::clone(&gateway);
            let arn = arn.clone();
            tasks.push(tokio::spawn(async move {
                let answer = delete_association(&gateway, &arn).await;
                (answer.status, answer.error)
            }));
        }
        let mut answers = Vec::with_capacity(racers);
        for task in tasks {
            answers.push(task.await.expect("the racing delete answered"));
        }

        let accepted = answers
            .iter()
            .filter(|(status, _)| *status == DELETED)
            .count();
        assert_eq!(
            accepted, 1,
            "case {case}: expected exactly one accepted delete, got {answers:?}"
        );
        for (status, error) in &answers {
            if *status == DELETED {
                continue;
            }
            assert_eq!(
                (*status, error.as_deref()),
                (404, Some(NOT_FOUND)),
                "case {case}: every loser is the modelled not-found, got {answers:?}"
            );
        }

        let listed = stored_arns(&gateway, &case).await;
        assert_eq!(
            listed,
            vec![sibling.clone()],
            "case {case}: expected only the sibling {sibling:?} to be left, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn the_collection_a_delete_leaves_is_what_a_later_reader_sees() {
    let gateway = Gateway::new();
    let domain = private_domain(&gateway, "stable.example.com").await;
    let mut arns: Vec<String> = Vec::new();
    for index in 0..4 {
        arns.push(
            stored_association(&gateway, &domain, &format!("vpce-0000000000000000{index}")).await,
        );
    }
    // Delete the middle record: the records around it keep their creation order, which is
    // what the collection pages over.
    assert_deleted(
        &delete_association(&gateway, &arns[1]).await,
        "the second of four associations",
    );
    let expected = vec![arns[0].clone(), arns[2].clone(), arns[3].clone()];
    let listed = stored_arns(&gateway, "the collection after the middle delete").await;
    assert_eq!(
        listed, expected,
        "case the collection after the middle delete: expected creation order {expected:?}, \
         got {listed:?}"
    );

    // Paging one record at a time reassembles the same sequence, and reading twice reports
    // the same page: the delete is what changed the collection, not the reads.
    let mut seen: Vec<String> = Vec::new();
    let mut position: Option<String> = None;
    let mut pages = 0;
    loop {
        let query = match &position {
            None => "limit=1".to_string(),
            Some(cursor) => format!("limit=1&position={cursor}"),
        };
        let answer = list_associations(&gateway, &query).await;
        assert_eq!(
            answer.status, 200,
            "case paging after the middle delete: expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        seen.extend(listed_arns(&answer.body, "paging after the middle delete"));
        pages += 1;
        assert!(
            pages <= 20,
            "case paging after the middle delete: pagination did not terminate"
        );
        match answer.body["position"].as_str() {
            None => break,
            Some(cursor) => position = Some(cursor.to_owned()),
        }
    }
    assert_eq!(
        seen, expected,
        "case paging after the middle delete: expected {expected:?}, got {seen:?}"
    );

    let once = list_associations(&gateway, "limit=500").await;
    let twice = list_associations(&gateway, "limit=500").await;
    assert_eq!(
        once.body, twice.body,
        "case the collection read twice: expected the same page, got {:?}",
        twice.body
    );
    assert!(
        !page_items(&once.body, "the collection read twice").is_empty(),
        "case the collection read twice: expected the records that are left, got {:?}",
        once.body
    );
}

/// RULE: a domain name access association is "a domain name access association between an
/// access association source and a private custom domain name", so the record IS that pair —
/// and the ARN that addresses it spells BOTH halves of it. So the
/// `domainNameAccessAssociationArn` a create reports is the pair's own ARN, and that one value
/// is what every later call that addresses the record resolves: the tag read, the delete, and
/// the collection that lists it.
///
/// The rows span what the rule generalises over: the shape of the private custom domain name
/// (a short name, a deeply nested name, a wildcard name, two private records carrying ONE
/// name — which are two domain names, so two different pairs) crossed with the two widths of
/// VPC endpoint id EC2 mints.
#[tokio::test]
async fn the_arn_a_create_reports_is_the_pair_the_association_is() {
    let gateway = Gateway::new();
    // The two widths of the hexadecimal part of a VPC endpoint id, from EC2's own id pattern
    // (`^vpce-([0-9a-f]{8}|[0-9a-f]{17})$`).
    let sources = ["vpce-0a1b2c3d", "vpce-0123456789abcdef0"];
    let names = [
        "a.example.com",
        "deeply.nested.private.api.example.co.uk",
        "*.example.net",
    ];

    let mut pairs: Vec<(String, String, String)> = Vec::new();
    for name in names {
        for source in sources {
            let domain = private_domain(&gateway, name).await;
            pairs.push((
                format!("{name} with {source}"),
                domain,
                (*source).to_string(),
            ));
        }
    }
    // Two private records carrying ONE name are two domain names — a `domainNameId` is
    // "supported only for private custom domain names" and is what identifies one — so the
    // same source with each of them is two different pairs.
    let shared_name = "shared.example.com";
    let first = private_domain(&gateway, shared_name).await;
    let second = private_domain(&gateway, shared_name).await;
    assert_ne!(
        first, second,
        "case two private records of one name: expected distinct domain names, got \
         {first:?} twice"
    );
    for domain in [first, second] {
        pairs.push((
            format!("{shared_name} ({domain}) with {}", sources[0]),
            domain,
            sources[0].to_string(),
        ));
    }

    let mut reported: Vec<String> = Vec::new();
    for (case, domain, source) in &pairs {
        let arn = stored_association(&gateway, domain, source).await;
        let expected = association_arn(&domain_name_key_of(domain), source);
        assert_eq!(
            arn, expected,
            "case {case}: expected the pair's own ARN {expected:?}, got {arn:?}"
        );
        // The ARN the create reported resolves the record it created, through the reader that
        // addresses one of these records by ARN.
        let tags = get_tags(&gateway, &arn).await;
        assert_eq!(
            tags.status, 200,
            "case GetTags({case}): expected 200, got {} with {:?}",
            tags.status, tags.body
        );
        reported.push(arn);
    }

    // Every pair got its own ARN, and the collection reports exactly those.
    let listed = stored_arns(&gateway, "every stored pair").await;
    assert_eq!(
        listed, reported,
        "case every stored pair: expected the reported ARNs {reported:?}, got {listed:?}"
    );

    // And the same value is what removes the record: after the delete the reader that
    // resolved it answers the modelled not-found for that very ARN.
    for (index, (case, _, _)) in pairs.iter().enumerate() {
        let arn = &reported[index];
        assert_deleted(&delete_association(&gateway, arn).await, case);
        let tags = get_tags(&gateway, arn).await;
        assert_failure(&tags, 404, NOT_FOUND, &format!("GetTags({case}) once deleted"));
        assert_eq!(
            tags.body["message"].as_str(),
            Some(NO_SUCH_ACCESS_ASSOCIATION),
            "case GetTags({case}) once deleted: expected {NO_SUCH_ACCESS_ASSOCIATION:?}, got \
             {:?}",
            tags.body["message"]
        );
    }
    let listed = stored_arns(&gateway, "every pair deleted").await;
    assert!(
        listed.is_empty(),
        "case every pair deleted: expected an empty page, got {listed:?}"
    );
}
