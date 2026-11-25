// SPDX-License-Identifier: CC0-1.0

//! The page member of a base path mapping listing, at EVERY count a domain name's collection
//! passes through as a caller fills it and empties it again.
//!
//! The rule under test: every `position`/`limit` collection of this service answers "the
//! current page of elements from this collection" as a LIST — present, and empty where the
//! page holds no element — so a client reads how many elements are listed off a list it can
//! count, at every count the collection holds. The base path mappings of a domain name are
//! that rule too: a domain name that never carried a mapping, one carrying several, and one a
//! caller has deleted the last mapping of are each answered the member, so a client walking a
//! custom domain's mappings never has to tell an absent member from an empty page.
//!
//! The rows differ along what the rule generalises over: how many mappings the domain name
//! carries (none, one, and several), the SPELLING of each base path (a plain one, one with a
//! digit, and the documented `(none)` a request spells by naming none at all), and the page
//! size the read asks for (none, the smallest a page may be, and the published maximum
//! "The maximum number of returned results per page. The default value is 25 and the maximum
//! value is 500", `smithy_query(query="doc", shape="GetBasePathMappingsRequest",
//! member="limit")`). The count is driven DOWN as well as up: each mapping is deleted in turn
//! and the listing is read again after every delete, so the member is asserted at each count
//! on the way to empty as well as on the way up.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client sees.
//! [`Gateway`], `page_items` and `string_member` are this crate's one wire-test harness;
//! `created_domain` is the domain-name suite's spelling of that create, `create_mapping` /
//! `deployed_api` the base-path-mapping suite's, `deleted` the emptied-collection suite's and
//! `empty_page_body` this crate's one reading of the body an empty page answers.

use serde_json::json;

use crate::base_path_mapping::{create_mapping, deployed_api};
use crate::domain_name_collection::created_domain;
use crate::emptied_collection_page_member::{deleted, empty_page_body};
use crate::rest_api_lifecycle::{page_items, string_member, Gateway};

/// The page sizes each read asks the collection for: none at all, the smallest a page may be,
/// and the published maximum.
const PAGE_SIZES: [&str; 3] = ["", "?limit=1", "?limit=500"];

/// The base paths a row maps, in creation order: a plain one, one carrying a digit, and the
/// documented `(none)` a request spells by naming no `basePath` at all.
const BASE_PATHS: [&str; 3] = ["orders", "v2", "(none)"];

/// Every base path the listing of `domain_name` reports, at each page size, asserting the page
/// member is PRESENT in every answer.
async fn listed_base_paths(gateway: &Gateway, domain_name: &str, case: &str) -> Vec<String> {
    let mut reported: Option<Vec<String>> = None;
    for size in PAGE_SIZES {
        let case = format!("{case} read with {size:?}");
        let answer = gateway
            .call(
                "GET",
                &format!("/domainnames/{domain_name}/basepathmappings{size}"),
                None,
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200 from GetBasePathMappings, got {} with {:?}",
            answer.status, answer.body
        );
        // The member is a list whatever the page holds: present, and empty for a page of
        // nothing.
        assert!(
            answer.body.get("item").is_some_and(|item| item.is_array()),
            "case {case}: expected the page member present as a list, got {:?}",
            answer.body
        );
        let paths: Vec<String> = page_items(&answer.body, &case)
            .iter()
            .map(|item| string_member(item, "basePath", &case))
            .collect();
        if paths.is_empty() {
            // The whole body of a page that holds nothing, so an empty collection cannot be
            // answered a second way.
            assert_eq!(
                answer.body,
                empty_page_body(),
                "case {case}: expected {:?}, got {:?}",
                empty_page_body(),
                answer.body
            );
        }
        match &reported {
            None => reported = Some(paths),
            // `limit=1` pages, so only the sizes that hold the whole collection are compared
            // against each other; what every size shares is the member being a list.
            Some(first) if size != "?limit=1" => assert_eq!(
                first, &paths,
                "case {case}: expected every page size holding the collection to report the \
                 same mappings, got {paths:?} after {first:?}"
            ),
            Some(_) => {}
        }
    }
    reported.unwrap_or_default()
}

/// RULE: the listing answers its page member as a list at every count the collection holds —
/// including before the first mapping is created and after the last one is deleted.
#[tokio::test]
async fn a_base_path_mapping_listing_reports_its_page_member_at_every_count() {
    for mappings in 0..=BASE_PATHS.len() {
        let gateway = Gateway::new();
        let (api, stage) = deployed_api(&gateway, "page-member-api", "prod").await;
        let domain_name = format!("page-member-{mappings}.example.com");
        created_domain(&gateway, &domain_name, json!({})).await;
        let case = format!("a domain name carrying {mappings} mapping(s)");

        // Before anything is mapped: the collection is empty and still reports its member.
        assert!(
            listed_base_paths(&gateway, &domain_name, &format!("{case}, before any create"))
                .await
                .is_empty(),
            "case {case}: a domain name that carries no mapping lists nothing"
        );

        let mut created: Vec<String> = Vec::new();
        for base_path in BASE_PATHS.iter().take(mappings) {
            // The documented `(none)` mapping is the one a request spells by naming no
            // `basePath` at all, so that row sends none.
            let body = if *base_path == "(none)" {
                json!({ "restApiId": api, "stage": stage })
            } else {
                json!({ "basePath": base_path, "restApiId": api, "stage": stage })
            };
            let answer = create_mapping(&gateway, &domain_name, body).await;
            assert_eq!(
                answer.status, 201,
                "case {case}: expected 201 from CreateBasePathMapping of {base_path:?}, got {} \
                 with {:?}",
                answer.status, answer.body
            );
            created.push(string_member(&answer.body, "basePath", &case));
            let listed =
                listed_base_paths(&gateway, &domain_name, &format!("{case}, after {base_path:?}"))
                    .await;
            assert_eq!(
                listed.len(),
                created.len(),
                "case {case}: expected {} mapping(s) listed after creating {base_path:?}, got \
                 {listed:?}",
                created.len()
            );
            for path in &created {
                assert!(
                    listed.contains(path),
                    "case {case}: expected {path:?} among the listed mappings, got {listed:?}"
                );
            }
        }

        // ...and down again: every delete leaves the member reporting exactly what is left,
        // the last one included, which is what a caller cleaning up after itself reads.
        while let Some(removed) = created.pop() {
            deleted(
                &gateway,
                &format!("/domainnames/{domain_name}/basepathmappings/{removed}"),
                &case,
            )
            .await;
            let listed =
                listed_base_paths(&gateway, &domain_name, &format!("{case}, after deleting"))
                    .await;
            assert!(
                !listed.contains(&removed),
                "case {case}: expected {removed:?} gone from the listing, got {listed:?}"
            );
            assert_eq!(
                listed.len(),
                created.len(),
                "case {case}: expected {} mapping(s) left after deleting {removed:?}, got \
                 {listed:?}",
                created.len()
            );
        }
    }
}
