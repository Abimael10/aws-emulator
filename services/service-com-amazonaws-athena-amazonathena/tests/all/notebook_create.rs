// SPDX-License-Identifier: CC0-1.0

//! `CreateNotebook`, driven end to end over the same router the emulator boots (awsJson1_1,
//! `x-amz-target: AmazonAthena.CreateNotebook`), and read back through the readers that
//! resolve a `NotebookId`.
//!
//! THE RULE UNDER TEST: a create mints a fresh `NotebookId` for an EMPTY ipynb file in a
//! Spark enabled workgroup that does not already hold a notebook of that name, and an
//! export of that id answers with that empty document beside the metadata the create stored;
//! a workgroup this account does not hold, one that is not Spark enabled, and a name the
//! workgroup already holds are each refused with `InvalidRequestException`, and a repeat
//! under one `ClientRequestToken` creates nothing further.
//!
//! Each rule is asserted over a TABLE rather than one request: the rows differ along the
//! shape and length of the notebook name, which precondition of the target workgroup fails,
//! and whether an idempotency token was reused for the same parameters or for others.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    self, DEFAULT_WORK_GROUP, EMPTY_IPYNB, ERROR_CODE_INVALID_INPUT, NOTEBOOK_TYPE_IPYNB,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, error_code, message, router};

/// The Spark enabled workgroup the notebooks are created in.
const WORK_GROUP: &str = "spark-notebooks";

/// A second Spark enabled workgroup, where the same notebook name is another notebook.
const OTHER_WORK_GROUP: &str = "spark-notebooks-two";

/// An Athena SQL workgroup: it runs no notebook engine, so it holds no notebooks.
const SQL_WORK_GROUP: &str = "sql-workgroup";

/// The PySpark engine version a Spark enabled workgroup selects.
const PYSPARK_ENGINE_VERSION: &str = "PySpark engine version 3";

/// Create a Spark enabled workgroup called `name`.
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

/// One `CreateNotebook` call, as a client sends it.
async fn create(router: &MultiServiceRouter, work_group: &str, name: &str) -> (u16, Value) {
    call(
        router,
        "CreateNotebook",
        &json!({ "WorkGroup": work_group, "Name": name }),
    )
    .await
}

/// `ExportNotebook`'s view of one notebook.
async fn export(router: &MultiServiceRouter, notebook_id: &str) -> (u16, Value) {
    call(router, "ExportNotebook", &json!({ "NotebookId": notebook_id })).await
}

/// Whether `id` has the modelled `NotebookId` form
/// (`^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$`).
fn is_notebook_id(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&groups)
            .all(|(len, group)| {
                group.len() == *len
                    && group
                        .chars()
                        .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character))
            })
}

/// A created notebook is an EMPTY ipynb file an export answers with, under a fresh id and
/// beside the metadata the create stored. Its id also resolves for the notebook's session
/// listing, which reports no session because nothing started one in it.
#[tokio::test]
async fn a_created_notebook_exports_the_empty_ipynb_document() {
    let router = router();
    given_spark_work_group(&router, WORK_GROUP).await;

    // Rows differ along the shape and the length of the name, within the modelled
    // `NotebookName` (len 1..255, and no `/`, `:` or `\`): a plain one, one carrying spaces
    // and an extension, a multi-byte one, the shortest the model admits and the longest.
    let names = [
        ("a plain name", "analysis".to_string()),
        ("spaces and an extension", "quarterly report.ipynb".to_string()),
        ("characters outside ASCII", "分析ノート".to_string()),
        ("the shortest name the model admits", "a".to_string()),
        ("the longest name the model admits", "n".repeat(255)),
    ];

    let mut minted: Vec<String> = Vec::new();
    for (case, name) in &names {
        let (status, created) = create(&router, WORK_GROUP, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateNotebook to answer 200, got {status} with {created:?}"
        );
        let notebook_id = created["NotebookId"].as_str().unwrap_or_default().to_string();
        assert!(
            is_notebook_id(&notebook_id),
            "case {case:?}: expected a NotebookId of the modelled form, got {notebook_id:?}"
        );
        assert!(
            !minted.contains(&notebook_id),
            "case {case:?}: expected an id no earlier create minted, got {notebook_id:?} among \
             {minted:?}"
        );
        minted.push(notebook_id.clone());

        let (status, exported) = export(&router, &notebook_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the created notebook must export, got {status} with {exported:?}"
        );
        assert_eq!(
            exported["Payload"].as_str(),
            Some(EMPTY_IPYNB),
            "case {case:?}: a created notebook holds the empty ipynb document, got {exported:?}"
        );
        let metadata = &exported["NotebookMetadata"];
        assert_eq!(
            metadata["NotebookId"].as_str(),
            Some(notebook_id.as_str()),
            "case {case:?}: the metadata names the created notebook, got {metadata:?}"
        );
        assert_eq!(
            metadata["Name"].as_str(),
            Some(name.as_str()),
            "case {case:?}: the metadata carries the name the create gave, got {metadata:?}"
        );
        assert_eq!(
            metadata["WorkGroup"].as_str(),
            Some(WORK_GROUP),
            "case {case:?}: the metadata names the workgroup created in, got {metadata:?}"
        );
        assert_eq!(
            metadata["Type"].as_str(),
            Some(NOTEBOOK_TYPE_IPYNB),
            "case {case:?}: the only notebook type is {NOTEBOOK_TYPE_IPYNB}, got {metadata:?}"
        );
        let created_at = metadata["CreationTime"].as_f64();
        assert!(
            created_at.is_some(),
            "case {case:?}: the metadata carries a CreationTime, got {metadata:?}"
        );
        assert_eq!(
            metadata["LastModifiedTime"].as_f64(),
            created_at,
            "case {case:?}: a notebook nothing has updated was last modified when it was \
             created, got {metadata:?}"
        );

        // The same id resolves for the notebook's session listing, which reports none
        // because nothing started a session in it.
        let (status, sessions) = call(
            &router,
            "ListNotebookSessions",
            &json!({ "NotebookId": notebook_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the created notebook's sessions must be listable, got {status} \
             with {sessions:?}"
        );
        assert_eq!(
            sessions["NotebookSessionsList"].as_array(),
            Some(&Vec::new()),
            "case {case:?}: a notebook nothing started a session in has none, got {sessions:?}"
        );
    }
}

/// A create needs a Spark enabled workgroup this account holds, with the name free: each
/// other case is refused, and the same name in ANOTHER Spark workgroup is another notebook.
#[tokio::test]
async fn a_create_needs_a_spark_enabled_workgroup_that_holds_the_name_free() {
    let router = router();
    given_spark_work_group(&router, WORK_GROUP).await;
    given_spark_work_group(&router, OTHER_WORK_GROUP).await;
    crate::harness::create_work_group(&router, SQL_WORK_GROUP).await;

    let (status, taken) = create(&router, WORK_GROUP, "taken").await;
    assert_eq!(
        status, 200,
        "the first create of a name must be accepted, got {status} with {taken:?}"
    );

    // Rows differ along WHICH precondition of the target workgroup fails: it does not
    // exist, it runs Athena SQL (named, or the account's default), or it already holds a
    // notebook of that name.
    let refusals: Vec<(&str, &str, &str, String)> = vec![
        (
            "a workgroup this account does not hold",
            "never-created",
            "new",
            state::WORK_GROUP_NOT_FOUND.to_string(),
        ),
        (
            "an Athena SQL workgroup",
            SQL_WORK_GROUP,
            "new",
            state::WORK_GROUP_ENGINE_VERSION_NOT_SPARK.to_string(),
        ),
        (
            "the account's default workgroup",
            DEFAULT_WORK_GROUP,
            "new",
            state::WORK_GROUP_ENGINE_VERSION_NOT_SPARK.to_string(),
        ),
        (
            "a name the workgroup already holds",
            WORK_GROUP,
            "taken",
            state::notebook_name_exists(WORK_GROUP, "taken"),
        ),
    ];

    for (case, work_group, name, expected) in refusals {
        let (status, refused) = create(&router, work_group, name).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got {refused:?}"
        );
        assert_eq!(
            message(&refused),
            expected,
            "case {case:?}: expected the sentence {expected:?}, got {refused:?}"
        );
        assert_eq!(
            refused["AthenaErrorCode"].as_str(),
            Some(ERROR_CODE_INVALID_INPUT),
            "case {case:?}: expected AthenaErrorCode {ERROR_CODE_INVALID_INPUT:?}, got \
             {refused:?}"
        );
    }

    // The same name in another Spark enabled workgroup is another notebook.
    let (status, elsewhere) = create(&router, OTHER_WORK_GROUP, "taken").await;
    assert_eq!(
        status, 200,
        "the same name in another workgroup must be accepted, got {status} with {elsewhere:?}"
    );
    assert_ne!(
        elsewhere["NotebookId"], taken["NotebookId"],
        "two notebooks of one name in two workgroups carry different ids, got {elsewhere:?} \
         and {taken:?}"
    );
}

/// `ClientRequestToken` makes a create idempotent: a repeat under one token answers with the
/// notebook that token already minted, and the same token for other parameters describes
/// another create, which is refused.
#[tokio::test]
async fn one_client_request_token_creates_one_notebook() {
    let router = router();
    given_spark_work_group(&router, WORK_GROUP).await;

    // The modelled `ClientRequestToken` is a hyphenated hex token
    // (`^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$`); the rows differ
    // along the name created under each.
    let tokens = [
        ("a plain name", "11111111-1111-4111-8111-111111111111", "idempotent"),
        (
            "a name outside ASCII",
            "22222222-2222-4222-8222-222222222222",
            "冪等性",
        ),
    ];

    for (case, token, name) in tokens {
        let request = json!({ "WorkGroup": WORK_GROUP, "Name": name, "ClientRequestToken": token });
        let (status, first) = call(&router, "CreateNotebook", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the first create to answer 200, got {status} with {first:?}"
        );
        let (status, again) = call(&router, "CreateNotebook", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a create repeated under one token executes once and answers the \
             same way, got {status} with {again:?}"
        );
        assert_eq!(
            again["NotebookId"], first["NotebookId"],
            "case {case:?}: the repeat answers with the id the token already minted, got \
             {again:?} after {first:?}"
        );

        // The same token for OTHER parameters describes another create, which is not the
        // one it already minted.
        let (status, mismatched) = call(
            &router,
            "CreateNotebook",
            &json!({
                "WorkGroup": WORK_GROUP,
                "Name": format!("{name}-changed"),
                "ClientRequestToken": token,
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a reused token under changed parameters to be refused, \
             got {status} with {mismatched:?}"
        );
        assert_eq!(
            message(&mismatched),
            state::IDEMPOTENT_PARAMETERS_DO_NOT_MATCH,
            "case {case:?}: expected the idempotency refusal, got {mismatched:?}"
        );

        // The repeat created nothing further: the notebook the token minted is still the
        // one that name resolves to, and the changed name resolves to nothing.
        let (status, exported) = export(
            &router,
            first["NotebookId"].as_str().unwrap_or_default(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the notebook the token minted must still export, got {status} \
             with {exported:?}"
        );
        assert_eq!(
            exported["NotebookMetadata"]["Name"].as_str(),
            Some(name),
            "case {case:?}: it must still carry the name it was created with, got {exported:?}"
        );
    }
}
