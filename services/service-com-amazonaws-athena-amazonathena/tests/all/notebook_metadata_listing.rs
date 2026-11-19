// SPDX-License-Identifier: CC0-1.0

//! Athena's NOTEBOOK METADATA readers, driven end to end over the same router the emulator
//! boots (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`): `ListNotebookMetadata`
//! "Displays the notebook files for the specified workgroup in paginated format" and
//! `GetNotebookMetadata` "Retrieves notebook metadata for the specified notebook ID".
//!
//! THE RULE UNDER TEST: both readers answer out of the notebooks a create or an import
//! persisted in the workgroup — the listing reports exactly the notebooks of the workgroup
//! addressed, narrowed by the name its `Filters` searches for, in one stable order and in
//! pages of at most the maximum asked for, and the single read reports the same metadata for
//! the notebook the listing reported. Neither reader invents a value: every member it reports
//! is one the create or import wrote.
//!
//! The rules are asserted over TABLES whose rows differ along what they generalise over: how
//! many notebooks a workgroup holds and which workgroup they belong to, the search string
//! (absent, a whole name, a shared infix, one differing only in case, one no name carries),
//! and the page size a listing asks for.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    self, page_token_malformed, ERROR_CODE_INVALID_INPUT, NOTEBOOK_TYPE_IPYNB,
    PYSPARK_ENGINE_VERSION,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call, create_work_group, router};

/// The Spark enabled workgroup whose notebooks are listed below.
const WORK_GROUP: &str = "spark-notebook-listing";

/// A second Spark enabled workgroup: a notebook belongs to the workgroup it was created in,
/// so this one's listing must never report the other's notebooks.
const OTHER_WORK_GROUP: &str = "spark-notebook-listing-other";

/// Create a Spark enabled workgroup called `name`: a PySpark engine version and the
/// execution role a Spark workgroup accesses user resources through.
async fn given_spark_work_group(router: &MultiServiceRouter, name: &str) {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": name,
            "Configuration": {
                "EngineVersion": { "SelectedEngineVersion": PYSPARK_ENGINE_VERSION },
                "ExecutionRole": "arn:aws:iam::000000000000:role/AthenaSparkExecution"
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the Spark workgroup {name} must be created, got {status} with {created:?}"
    );
}

/// Create the notebook `name` in `work_group` and hand back the `NotebookId` it minted.
async fn given_notebook(router: &MultiServiceRouter, work_group: &str, name: &str) -> String {
    let (status, created) = call(
        router,
        "CreateNotebook",
        &json!({ "WorkGroup": work_group, "Name": name }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the notebook {name} must be created in {work_group}, got {status} with {created:?}"
    );
    created["NotebookId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateNotebook must answer with an id, got {created:?}"))
        .to_string()
}

/// One page of `ListNotebookMetadata`, as the whole answer.
async fn list(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "ListNotebookMetadata", body).await
}

/// The `(NotebookId, Name)` pairs one page reported, in the order it gave them.
fn listed_names(page: &Value) -> Vec<(String, String)> {
    page["NotebookMetadataList"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|entry| {
            (
                entry["NotebookId"].as_str().unwrap_or_default().to_string(),
                entry["Name"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// A workgroup's listing reports exactly its own notebooks, and every entry it reports is
/// the metadata the single reader reports for that same notebook.
#[tokio::test]
async fn a_listing_reports_the_workgroups_own_notebooks_and_each_reads_back() {
    let router = router();
    given_spark_work_group(&router, WORK_GROUP).await;
    given_spark_work_group(&router, OTHER_WORK_GROUP).await;

    // Rows differ along the NAME's shape: a plain one, one at the modelled `NotebookName`
    // floor (one character), one at its ceiling (255), and one carrying a space and a dot.
    let longest = "n".repeat(255);
    let names: Vec<String> = vec![
        "analysis.ipynb".to_string(),
        "n".to_string(),
        longest.clone(),
        "my notebook.ipynb".to_string(),
    ];
    let mut minted: Vec<(String, String)> = Vec::new();
    for name in &names {
        minted.push((given_notebook(&router, WORK_GROUP, name).await, name.clone()));
    }
    // One in the OTHER workgroup, under a name the first workgroup also holds: the same name
    // in another workgroup is another notebook.
    let elsewhere = given_notebook(&router, OTHER_WORK_GROUP, "analysis.ipynb").await;

    let (status, page) = list(&router, &json!({ "WorkGroup": WORK_GROUP })).await;
    assert_eq!(
        status, 200,
        "the workgroup's notebooks must be listable, got {status} with {page:?}"
    );
    let reported = listed_names(&page);
    assert_eq!(
        reported.len(),
        names.len(),
        "the listing must report every notebook of the workgroup and nothing else, got \
         {reported:?}"
    );
    assert_eq!(
        page["NextToken"],
        Value::Null,
        "a listing that reported everything hands back no token, got {page:?}"
    );
    for (id, name) in &minted {
        assert!(
            reported.iter().any(|(listed, _)| listed == id),
            "the notebook {name:?} ({id}) must be in the listing, got {reported:?}"
        );
    }
    assert!(
        !reported.iter().any(|(id, _)| *id == elsewhere),
        "the other workgroup's notebook must not be in this listing, got {reported:?}"
    );

    // The order is the same on every call: the page boundary a paged caller sees cannot
    // depend on which call it was.
    let (_, again) = list(&router, &json!({ "WorkGroup": WORK_GROUP })).await;
    assert_eq!(
        listed_names(&again),
        reported,
        "two listings of the same workgroup must report the same order, got {again:?}"
    );

    // Every entry the listing reported reads back identically through the single reader,
    // which is the round trip of the create that minted it.
    for (id, name) in &minted {
        let (status, read) = call(
            &router,
            "GetNotebookMetadata",
            &json!({ "NotebookId": id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: the notebook's metadata must read back, got {status} with {read:?}"
        );
        let metadata = read["NotebookMetadata"].clone();
        assert_eq!(
            metadata["Name"].as_str(),
            Some(name.as_str()),
            "case {name:?}: the read reports the name the create carried, got {metadata:?}"
        );
        assert_eq!(
            metadata["WorkGroup"].as_str(),
            Some(WORK_GROUP),
            "case {name:?}: the read reports the workgroup it was created in, got {metadata:?}"
        );
        assert_eq!(
            metadata["Type"].as_str(),
            Some(NOTEBOOK_TYPE_IPYNB),
            "case {name:?}: the only notebook type the model declares, got {metadata:?}"
        );
        assert!(
            metadata["CreationTime"].as_f64().is_some(),
            "case {name:?}: the read reports when it was created, got {metadata:?}"
        );
        let entry = page["NotebookMetadataList"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .find(|entry| entry["NotebookId"].as_str() == Some(id.as_str()))
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            entry, metadata,
            "case {name:?}: the listing and the single read must report the same metadata"
        );
    }

    // A workgroup that holds none reports the empty list rather than a refusal.
    given_spark_work_group(&router, "spark-notebook-listing-empty").await;
    let (status, empty) = list(
        &router,
        &json!({ "WorkGroup": "spark-notebook-listing-empty" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a Spark workgroup holding no notebook must still list, got {status} with {empty:?}"
    );
    assert_eq!(
        listed_names(&empty),
        Vec::new(),
        "a workgroup holding no notebook reports none, got {empty:?}"
    );
}

/// `Filters.Name` searches the workgroup's notebook names: the notebooks reported are those
/// whose stored name carries the string, and the search is case-sensitive as every other
/// comparison of a notebook name in this service is.
#[tokio::test]
async fn the_name_filter_searches_the_stored_notebook_names() {
    let router = router();
    given_spark_work_group(&router, WORK_GROUP).await;
    for name in ["sales.ipynb", "sales-weekly.ipynb", "Sales-upper.ipynb"] {
        given_notebook(&router, WORK_GROUP, name).await;
    }

    // (the case, the search string, the names the search must report)
    let cases: Vec<(&str, Option<&str>, Vec<&str>)> = vec![
        (
            "no filter at all",
            None,
            vec!["sales.ipynb", "sales-weekly.ipynb", "Sales-upper.ipynb"],
        ),
        ("a whole name", Some("sales.ipynb"), vec!["sales.ipynb"]),
        (
            "an infix two names share",
            Some("sales-"),
            vec!["sales-weekly.ipynb"],
        ),
        (
            "an infix that is not at the start",
            Some("weekly"),
            vec!["sales-weekly.ipynb"],
        ),
        (
            "the same string in another case",
            Some("Sales"),
            vec!["Sales-upper.ipynb"],
        ),
        ("a string no name carries", Some("marketing"), vec![]),
        (
            "the extension every name ends in",
            Some(".ipynb"),
            vec!["sales.ipynb", "sales-weekly.ipynb", "Sales-upper.ipynb"],
        ),
    ];

    for (case, searched, expected) in cases {
        let mut body = json!({ "WorkGroup": WORK_GROUP });
        if let (Some(body), Some(searched)) = (body.as_object_mut(), searched) {
            body.insert("Filters".to_string(), json!({ "Name": searched }));
        }
        let (status, page) = list(&router, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the search must be served, got {status} with {page:?}"
        );
        let mut reported: Vec<String> = listed_names(&page)
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        reported.sort();
        let mut wanted: Vec<String> = expected.iter().map(|name| (*name).to_string()).collect();
        wanted.sort();
        assert_eq!(
            reported, wanted,
            "case {case:?}: expected {wanted:?}, got {reported:?}"
        );
    }
}

/// A listing is paged: a page carries at most the maximum asked for, the token it hands back
/// resumes exactly where it stopped, the last page hands back none, and a token this store
/// did not mint for this workgroup's listing is refused.
#[tokio::test]
async fn a_listing_is_paged_and_a_token_it_did_not_mint_is_refused() {
    let router = router();
    given_spark_work_group(&router, WORK_GROUP).await;
    given_spark_work_group(&router, OTHER_WORK_GROUP).await;
    for index in 0..5 {
        given_notebook(&router, WORK_GROUP, &format!("paged-{index}.ipynb")).await;
    }
    given_notebook(&router, OTHER_WORK_GROUP, "elsewhere.ipynb").await;

    let (_, whole) = list(&router, &json!({ "WorkGroup": WORK_GROUP })).await;
    let order = listed_names(&whole);
    assert_eq!(
        order.len(),
        5,
        "the five notebooks must all be reported unpaged, got {order:?}"
    );

    // Rows differ along the page size: one at a time, two at a time, exactly as many as are
    // held, and the modelled maximum — each must walk the same order with no entry repeated
    // or skipped.
    for page_size in [1_usize, 2, 5, 50] {
        let mut walked: Vec<(String, String)> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0_usize;
        loop {
            let mut body = json!({ "WorkGroup": WORK_GROUP, "MaxResults": page_size });
            if let (Some(body), Some(token)) = (body.as_object_mut(), token.as_deref()) {
                body.insert("NextToken".to_string(), json!(token));
            }
            let (status, page) = list(&router, &body).await;
            assert_eq!(
                status, 200,
                "case {page_size}: a page must be served, got {status} with {page:?}"
            );
            let reported = listed_names(&page);
            assert!(
                reported.len() <= page_size,
                "case {page_size}: a page carries at most the maximum asked for, got \
                 {reported:?}"
            );
            walked.extend(reported);
            pages += 1;
            assert!(
                pages <= 10,
                "case {page_size}: the walk must end, got {walked:?}"
            );
            match page["NextToken"].as_str() {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(
            walked, order,
            "case {page_size}: paging must walk the unpaged order exactly once"
        );
    }

    // A token minted for ANOTHER workgroup's listing names no page of this one, and neither
    // does a value nothing minted.
    let (_, first) = list(&router, &json!({ "WorkGroup": OTHER_WORK_GROUP, "MaxResults": 1 })).await;
    let foreign = first["NextToken"].as_str().map(str::to_string);
    let mut rejected: Vec<(&str, String)> = vec![
        ("a token no store minted", "not-a-token".to_string()),
        ("hexadecimal of the right shape", "00".repeat(40)),
    ];
    if let Some(foreign) = foreign {
        rejected.push(("a token minted for another workgroup", foreign));
    }
    for (case, token) in rejected {
        let (status, refused) = list(
            &router,
            &json!({ "WorkGroup": WORK_GROUP, "NextToken": token }),
        )
        .await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_INVALID_INPUT,
            &page_token_malformed(&token),
        );
    }

    // A workgroup this account does not hold holds no notebooks either, which is the miss
    // every operation of this service that resolves a `WorkGroup` states.
    create_work_group(&router, "sql-notebook-listing").await;
    let (status, refused) = list(&router, &json!({ "WorkGroup": "never-created" })).await;
    assert_refusal(
        "a workgroup this account does not hold",
        status,
        &refused,
        ERROR_CODE_INVALID_INPUT,
        state::WORK_GROUP_NOT_FOUND,
    );
}
