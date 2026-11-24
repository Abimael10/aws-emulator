// SPDX-License-Identifier: CC0-1.0

//! The baseline the replacement suite beside this one is read against: a GraphQL API's whole
//! estate, created and then deleted with no revision in between.
//!
//! RULE (the teardown half of [`crate::replaced_resource_minted_identity`]'s): "CloudFormation
//! deletes the resources in reverse order", so a resource is deleted before the resources it
//! depends on — the resolver before the data source its `DataSourceName` reads — and every one
//! of those deletes is addressed at the resource the owning service holds, so the stack reaches
//! `DELETE_COMPLETE` and AppSync is left holding nothing of the estate.
//!
//! One case, deliberately: what it adds to that suite is the deployment with NOTHING replaced,
//! whose records are the ones the create's own walk wrote — so a teardown that fails only for a
//! replaced resource's record is told apart from one that was never ordered right at all.

use crate::harness::{
    call, create_stack, member, router, settled_stack, stack_event_reasons, xml_text, REGION,
};
use crate::replaced_resource_minted_identity::{
    appsync, outputs_of, template, Revision, ONE_FIELD,
};

#[tokio::test]
async fn an_estate_deleted_without_a_revision_is_torn_down_completely() {
    let case = "an estate deleted straight after its create";
    let router = router();
    let stack = "appsync-estate-plain-teardown";
    let revision = Revision {
        definition: ONE_FIELD,
        field: "first",
        source: "NoneSource",
        description: "v1",
    };

    let (status, body) = create_stack(&router, stack, &template(&revision), &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {body}"
    );
    let described = settled_stack(case, &router, REGION, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to create, the events said {:?}",
        stack_event_reasons(&router, REGION, stack).await
    );
    let api_id = outputs_of(case, &described)
        .get("ApiIdOut")
        .cloned()
        .unwrap_or_default();

    let (status, body) = call(&router, "DeleteStack", &[member("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with {body}"
    );
    let deleted = settled_stack(case, &router, REGION, stack).await;
    let reported = xml_text(&deleted, "StackStatus").unwrap_or_default();
    assert!(
        deleted.is_empty() || reported == "DELETE_COMPLETE",
        "case {case:?}: expected the stack to delete, got {reported:?}; the events said {:?}",
        stack_event_reasons(&router, REGION, stack).await
    );
    let (status, read) = appsync(&router, &format!("/v1/apis/{api_id}")).await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected AppSync to hold no API once the stack was deleted, got {status} \
         with {read}"
    );
}
