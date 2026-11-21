// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for how this service reports a resource IT owns: the built-in
//! browser and code interpreter tools (`aws.browser.v1`, `aws.codeinterpreter.v1`) every
//! account holds without anything having created one.
//!
//! The rule under test: a resource the SERVICE owns is reported as the service's own — its
//! ARN carries the service-owned account segment `aws` with the bare resource type
//! (`browser` / `code-interpreter`), and the name and description it reports are the ones
//! the service publishes for that tool — while a resource an ACCOUNT created is reported
//! under that account's id with the `-custom` resource type and the name and description
//! its create carried. `BrowserArn` and `CodeInterpreterArn` admit both account segments
//! (`:(aws|[0-9]{12}):`, from `smithy_query(query="shape",
//! shape="BrowserArn,CodeInterpreterArn")`), and which one an ARN carries is decided by who
//! owns the resource, not by which caller is reading it.
//!
//! The rule is asserted over both tools and both owners, through the per-resource read
//! (`GetBrowser` / `GetCodeInterpreter`) and through the inventory (`ListBrowsers`), in
//! several Regions — the ARN carries the Region the request was signed for, so one Region
//! could not tell a baked-in ARN from a built one.
//!
//! The harness is `system_tools`'s: the two tools' routes, response members, ARN forms and
//! create bodies are that suite's table, so the inventory cannot describe the tools
//! differently from the reads and deletes asserted there.

use serde_json::json;

use crate::system_tools::{
    account_owned_arn, assert_date_time, browser_summary, create_tool, get_tool, list_browsers,
    router, service_owned_arn, text, tools, ACCOUNT, REGIONS,
};

/// A tool the SERVICE owns reports the name and the description the service publishes for
/// it, under the service's own account segment — in every Region, and whether or not the
/// account has a tool of its own beside it, whose name and description are the ones ITS
/// create supplied.
#[tokio::test]
async fn a_service_owned_tool_reports_the_values_the_service_publishes_for_it() {
    let router = router();

    for tool in tools() {
        for (index, region) in REGIONS.iter().enumerate() {
            let case = format!("{} in {region}", tool.case);
            let (status, read) = get_tool(&router, &tool, tool.reserved_id, region).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from the read, got {status} ({read})"
            );
            assert_eq!(
                text(&read, "name"),
                tool.published_name,
                "case {case:?}: expected the name the service publishes for the tool, got {}",
                text(&read, "name")
            );
            match tool.published_description {
                Some(description) => assert_eq!(
                    text(&read, "description"),
                    description,
                    "case {case:?}: expected the description the service publishes for the \
                     tool, got {}",
                    text(&read, "description")
                ),
                None => assert!(
                    read["description"].is_null(),
                    "case {case:?}: the service publishes no description for this tool, got {}",
                    read["description"]
                ),
            }
            let arn = text(&read, tool.arn_member);
            assert_eq!(
                arn,
                service_owned_arn(region, tool.arn_resource, tool.reserved_id),
                "case {case:?}: expected the service-owned ARN, got {arn:?}"
            );
            assert!(
                !arn.contains(&format!(":{ACCOUNT}:")),
                "case {case:?}: a tool the service owns names no account of the caller's, \
                 got {arn:?}"
            );

            // A tool of the account's own, beside it: the create's values and the account's
            // own ARN, which is what the service-owned form is NOT.
            let name = format!("ownValues{index}");
            let mut body = (tool.create_body)(&name);
            body["description"] = json!("a tool of this account's own");
            let (status, created) = create_tool(&router, &tool, &body, region).await;
            assert_eq!(
                status, 202,
                "case {case:?}: expected HTTP 202 from the create, got {status} ({created})"
            );
            let created_id = text(&created, tool.id_member);
            assert_eq!(
                text(&created, tool.arn_member),
                account_owned_arn(region, tool.arn_resource, &created_id),
                "case {case:?}: expected the account-owned ARN, got {}",
                text(&created, tool.arn_member)
            );
            let (status, read_back) = get_tool(&router, &tool, &created_id, region).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the created tool is readable by its minted id, got {status} \
                 ({read_back})"
            );
            assert_eq!(
                (text(&read_back, "name"), text(&read_back, "description")),
                (name.clone(), "a tool of this account's own".to_string()),
                "case {case:?}: the created tool reports the values the create carried, got \
                 {} / {}",
                text(&read_back, "name"),
                text(&read_back, "description")
            );

            // And the tool the service owns is untouched by that create.
            let (status, again) = get_tool(&router, &tool, tool.reserved_id, region).await;
            assert_eq!(
                (status, again),
                (200, read),
                "case {case:?}: a create of this account's own changes nothing about the \
                 service-owned tool"
            );
        }
    }
}

/// The inventory reports the tool the SERVICE owns exactly as a read of it does — ARN, name,
/// description and status — beside the browsers the account created, which carry the
/// account's own id and the `-custom` resource type; and every `DateTimestamp` it reports is
/// in the wire form its `@timestampFormat` names, for either owner.
///
/// One row per value `ResourceType` takes plus the unfiltered listing ("The type of browsers
/// to list. If not specified, all browser types are returned"): `SYSTEM` is the tool the
/// service owns, `CUSTOM` what this account created.
///
/// `ListBrowsers` is the only inventory of the two tools this service serves —
/// `ListCodeInterpreters` is still a scaffold stub answering 501 — so the rows below are the
/// browser's.
#[tokio::test]
async fn the_inventory_reports_the_service_owned_tool_as_the_read_does() {
    let router = router();
    let region = REGIONS[1];
    let [browser, _] = tools();

    let mut body = (browser.create_body)("inventoryProbe");
    body["description"] = json!("a browser of this account's own");
    let (status, created) = create_tool(&router, &browser, &body, region).await;
    assert_eq!(
        status, 202,
        "case \"the account's own browser\": expected HTTP 202 from the create, got {status} \
         ({created})"
    );
    let created_id = text(&created, browser.id_member);

    let (status, read) = get_tool(&router, &browser, browser.reserved_id, region).await;
    assert_eq!(
        status, 200,
        "case \"the service-owned browser read\": expected HTTP 200, got {status} ({read})"
    );

    // (case, the `type` filter, whether the service-owned tool is listed, whether the
    // account's own browser is listed)
    let rows = [
        ("no type filter", None, true, true),
        ("type=SYSTEM", Some("SYSTEM"), true, false),
        ("type=CUSTOM", Some("CUSTOM"), false, true),
    ];

    for (case, kind, lists_service_owned, lists_created) in rows {
        let (status, listing) = list_browsers(&router, region, kind).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListBrowsers, got {status} ({listing})"
        );

        let service_owned = browser_summary(&listing, browser.reserved_id);
        assert_eq!(
            service_owned.is_some(),
            lists_service_owned,
            "case {case:?}: expected the service-owned browser listed={lists_service_owned}, \
             got {listing}"
        );
        if let Some(summary) = service_owned {
            assert_eq!(
                text(&summary, browser.arn_member),
                service_owned_arn(region, browser.arn_resource, browser.reserved_id),
                "case {case:?}: expected the service-owned ARN in the inventory, got {}",
                text(&summary, browser.arn_member)
            );
            for member in ["name", "description", "status"] {
                assert_eq!(
                    text(&summary, member),
                    text(&read, member),
                    "case {case:?}: the inventory and the read must report one browser alike, \
                     expected {member}={:?}, got {:?}",
                    text(&read, member),
                    text(&summary, member)
                );
            }
            assert_eq!(
                text(&summary, "name"),
                browser.published_name,
                "case {case:?}: expected the name the service publishes for the tool, got {}",
                text(&summary, "name")
            );
            assert_eq!(
                text(&summary, "description"),
                browser.published_description.unwrap_or_default(),
                "case {case:?}: expected the description the service publishes for the tool, \
                 got {}",
                text(&summary, "description")
            );
            // `BrowserSummary.createdAt` is `@required` and targets `DateTimestamp`, which
            // carries `@timestampFormat("date-time")` (`smithy_query(query="shape",
            // shape="BrowserSummary")`), so the inventory reports it in that wire form — for
            // either owner.
            assert_date_time(case, "createdAt", &summary["createdAt"]);
        }

        let own = browser_summary(&listing, &created_id);
        assert_eq!(
            own.is_some(),
            lists_created,
            "case {case:?}: expected the account's own browser listed={lists_created}, got \
             {listing}"
        );
        if let Some(summary) = own {
            assert_eq!(
                text(&summary, browser.arn_member),
                account_owned_arn(region, browser.arn_resource, &created_id),
                "case {case:?}: expected the account-owned ARN in the inventory, got {}",
                text(&summary, browser.arn_member)
            );
            assert_eq!(
                (text(&summary, "name"), text(&summary, "description")),
                (
                    "inventoryProbe".to_string(),
                    "a browser of this account's own".to_string()
                ),
                "case {case:?}: the inventory reports the values the create carried, got {} / {}",
                text(&summary, "name"),
                text(&summary, "description")
            );
            assert_date_time(case, "createdAt", &summary["createdAt"]);
        }
    }
}
