// SPDX-License-Identifier: CC0-1.0

//! The schema a MERGED API holds, end to end over HTTP: what the merges of SEVERAL source
//! APIs COMPOSE into, and what a disassociation WITHDRAWS from it.
//!
//! One rule is asserted here, over tables spanning the classes it generalises over rather
//! than over the single pair of source APIs that exposed the defect: a Merged API's schema
//! is the composition of the schemas of every source API associated with it. Three
//! consequences follow, one test each:
//!
//! 1. a type NAME several source APIs declare is held once on the Merged API, carrying
//!    what all of them declare under it — the root operation types every source API
//!    declares its own fields on, and equally any other name two of them share — and a
//!    member two sources declare identically is carried once; the rows span one, two and
//!    three source APIs, a shared root type, a shared object type, a shared enum, an
//!    identical duplicate declaration, and a merge run a second time through
//!    `StartSchemaMerge`;
//! 2. a declaration that DISAGREES with what the Merged API already holds — one member
//!    declared differently, or one name declared as another kind — composes with nothing,
//!    so that merge is not performed: the association reports the modelled `MERGE_FAILED`
//!    (source: `smithy_query(query="shape", shape="SourceApiAssociationStatus")`) with a
//!    detail naming the source API, the association and what the two disagree about, and
//!    the Merged API keeps the schema it held;
//! 3. the types an association contributed leave the Merged API WITH the association: once
//!    its deletion has been materialised the Merged API holds exactly the composition of
//!    the associations it has left, so a type only the withdrawn source declared is refused
//!    404 `NotFoundException` and a root type two sources declared keeps only the remaining
//!    source's fields — from either side the deletion can be asked for.
//!
//! Every wait is a wait on the CONDITION the clock owes, never on a fixed time, and every
//! assertion goes through the router the production emulator mounts. The harness
//! (`router`, `call`, `create_api`, `get_path`, `await_terminal_merge`) is the association
//! suites', and the schema helpers (`start_creation`, `await_creation`, `get_type`) the
//! schema suite's, reused rather than copied.

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::schema_creation::{await_creation, get_type, start_creation};
use crate::source_api_association::{
    await_terminal_merge, call, create_api, get_path, router, Answer, MERGE_DEADLINE, POLL_SPACING,
};

/// A source API that declares its own field on the root `Query` type, as every source API
/// of a Merged API does.
const QUERY_ONE: &str = "type Query {\n  greetingOne: String\n}\n";

/// A second one, whose field has a name of its own: two sources declaring one root type
/// with different fields is the composition, not a conflict.
const QUERY_TWO: &str = "type Query {\n  greetingTwo: String\n}\n";

/// A third, so the composition is asserted past the two the defect was found with.
const QUERY_THREE: &str = "type Query {\n  greetingThree: String\n}\n";

/// A source API declaring fields on two root operation types at once.
const ROOTS_ONE: &str = "type Query {\n  greetingOne: String\n}\ntype Mutation {\n  putOne: String\n}\n";

/// The other side of that pair.
const ROOTS_TWO: &str = "type Query {\n  greetingTwo: String\n}\ntype Mutation {\n  putTwo: String\n}\n";

/// A source API declaring a NON-root object type the other also declares: the rule is
/// about the type name, not about which name it is.
const SHARED_OBJECT_ONE: &str =
    "type Query {\n  sharedAlpha: Shared\n}\ntype Shared {\n  alpha: String\n}\n";

/// The other declaration of that name, with a field of its own.
const SHARED_OBJECT_TWO: &str =
    "type Query {\n  sharedBeta: Shared\n}\ntype Shared {\n  beta: String\n}\n";

/// A source API declaring a type the other declares IDENTICALLY: both say the same thing,
/// so the composed type carries that member once.
const IDENTICAL_ONE: &str =
    "type Query {\n  widgetOne: Widget\n}\ntype Widget {\n  widgetId: ID!\n}\n";

/// The other API with the same `Widget` declaration under a root field of its own.
const IDENTICAL_TWO: &str =
    "type Query {\n  widgetTwo: Widget\n}\ntype Widget {\n  widgetId: ID!\n}\n";

/// A source API declaring an enum the other also declares, with a value of its own: a
/// kind whose members are names rather than fields.
const SHARED_ENUM_ONE: &str = "type Query {\n  colorOne: Color\n}\nenum Color {\n  RED\n}\n";

/// The other declaration of that enum.
const SHARED_ENUM_TWO: &str = "type Query {\n  colorTwo: Color\n}\nenum Color {\n  GREEN\n}\n";

/// One row of the composition table: the source APIs merged into one Merged API, and what
/// the Merged API must then hold.
#[derive(Debug)]
struct Composition {
    label: &'static str,
    /// One schema document per source API associated with the Merged API, merged in this
    /// order.
    sources: &'static [&'static str],
    /// What the Merged API holds afterwards: each type name, and the member text its
    /// definition must carry EXACTLY ONCE.
    expected: &'static [(&'static str, &'static [&'static str])],
    /// Whether every association is merged AGAIN through `StartSchemaMerge` once its
    /// first merge has settled: a merge carries the same schema twice, which composes to
    /// the same type system.
    remerge: bool,
}

/// The composition table: one source API, the two the defect was found with, three, a
/// shared non-root object type, a shared enum, two identical declarations of one type, two
/// root operation types shared at once, and a re-run merge.
fn compositions() -> Vec<Composition> {
    vec![
        Composition {
            label: "one source api",
            sources: &[QUERY_ONE],
            expected: &[("Query", &["greetingOne"])],
            remerge: false,
        },
        Composition {
            label: "two source apis sharing the root query type",
            sources: &[QUERY_ONE, QUERY_TWO],
            expected: &[("Query", &["greetingOne", "greetingTwo"])],
            remerge: false,
        },
        Composition {
            label: "three source apis sharing the root query type",
            sources: &[QUERY_ONE, QUERY_TWO, QUERY_THREE],
            expected: &[("Query", &["greetingOne", "greetingTwo", "greetingThree"])],
            remerge: false,
        },
        Composition {
            label: "two source apis sharing two root operation types",
            sources: &[ROOTS_ONE, ROOTS_TWO],
            expected: &[
                ("Query", &["greetingOne", "greetingTwo"]),
                ("Mutation", &["putOne", "putTwo"]),
            ],
            remerge: false,
        },
        Composition {
            label: "two source apis sharing a non-root object type",
            sources: &[SHARED_OBJECT_ONE, SHARED_OBJECT_TWO],
            expected: &[
                ("Query", &["sharedAlpha", "sharedBeta"]),
                ("Shared", &["alpha", "beta"]),
            ],
            remerge: false,
        },
        Composition {
            label: "two source apis sharing an enum type",
            sources: &[SHARED_ENUM_ONE, SHARED_ENUM_TWO],
            expected: &[
                ("Query", &["colorOne", "colorTwo"]),
                ("Color", &["RED", "GREEN"]),
            ],
            remerge: false,
        },
        Composition {
            label: "two source apis declaring one type identically",
            sources: &[IDENTICAL_ONE, IDENTICAL_TWO],
            expected: &[
                ("Query", &["widgetOne", "widgetTwo"]),
                ("Widget", &["widgetId"]),
            ],
            remerge: false,
        },
        Composition {
            label: "two source apis sharing the root query type, merged again",
            sources: &[QUERY_ONE, QUERY_TWO],
            expected: &[("Query", &["greetingOne", "greetingTwo"])],
            remerge: true,
        },
    ]
}

/// One merged source API of a fixture: the ids a row reads it back by.
#[derive(Debug, Clone)]
struct Merged {
    source_api_id: String,
    association_id: String,
}

/// `AssociateSourceGraphqlApi`'s path: the association collection of a Merged API.
fn associations_path(merged_api_id: &str) -> String {
    format!("/v1/mergedApis/{merged_api_id}/sourceApiAssociations")
}

/// `StartSchemaMerge`'s path.
fn merge_path(merged_api_id: &str, association_id: &str) -> String {
    format!("{}/{association_id}/merge", associations_path(merged_api_id))
}

/// One row's whole fixture: its own engine, the Merged API, and the associations whose
/// merges the clock owes.
struct Fixture {
    /// A router — one engine — of this row's own, so a row is written against a store
    /// holding only its own APIs and no row is bounded by what another one created.
    router: MultiServiceRouter,
    merged_api_id: String,
    associations: Vec<Merged>,
}

/// Create one Merged API and the source APIs holding `schemas`, start every source's
/// schema creation and associate every source with the Merged API — WITHOUT waiting for
/// any of it.
///
/// Nothing is waited for here, so the spans the clock owes a whole table run alongside
/// each other rather than one after another: a merge scheduled while the source API's own
/// schema creation is still owed comes due AFTER that creation, so the association reaches
/// its terminal status with the source's types either way. The sources are associated in
/// the order `schemas` lists them and each association's merge is due one span after its
/// own source's creation, so the merges come due in that same order — which is the order
/// the Merged API's schema is composed in.
async fn scheduled_fixture(label: &str, schemas: &[&str]) -> Fixture {
    let router = router();
    let merged = create_api(&router, &format!("{label}-merged"), state::MERGED_API_TYPE).await;
    let mut associations: Vec<Merged> = Vec::new();
    for (index, schema) in schemas.iter().enumerate() {
        let source_api_id = create_source(&router, label, index, schema).await;
        associations.push(associate(&router, label, index, &merged.id, &source_api_id).await);
    }
    Fixture {
        router,
        merged_api_id: merged.id,
        associations,
    }
}

/// Let every merge the row's associations scheduled come due, and answer each one's
/// settled association record in the order the sources were associated.
async fn await_merges(fixture: &Fixture, label: &str) -> Vec<Value> {
    let mut settled: Vec<Value> = Vec::new();
    for association in &fixture.associations {
        settled.push(
            await_terminal_merge(
                &fixture.router,
                label,
                &fixture.merged_api_id,
                &association.association_id,
            )
            .await,
        );
    }
    settled
}

/// Create one source API and start the creation of the schema `schema` on it.
async fn create_source(
    router: &MultiServiceRouter,
    label: &str,
    index: usize,
    schema: &str,
) -> String {
    let source = create_api(
        router,
        &format!("{label}-source-{index}"),
        state::GRAPHQL_API_TYPE,
    )
    .await;
    let started = start_creation(router, &source.id, schema.as_bytes()).await;
    assert_eq!(
        started.status, 200,
        "case {label:?} source {index}: expected StartSchemaCreation 200, got {} {:?}",
        started.status, started.body
    );
    source.id
}

/// Associate the source API with the Merged API, which SCHEDULES the merge of its schema.
async fn associate(
    router: &MultiServiceRouter,
    label: &str,
    index: usize,
    merged_api_id: &str,
    source_api_id: &str,
) -> Merged {
    let created = call(
        router,
        "POST",
        &associations_path(merged_api_id),
        Some(json!({"sourceApiIdentifier": source_api_id})),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {label:?} source {index}: expected the association to be created, got {} {:?}",
        created.status, created.body
    );
    Merged {
        source_api_id: source_api_id.to_string(),
        association_id: created.body["sourceApiAssociation"]["associationId"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    }
}

/// The definition the Merged API reports for `type_name`, asserting the read succeeded.
async fn merged_definition(
    router: &MultiServiceRouter,
    label: &str,
    merged_api_id: &str,
    type_name: &str,
) -> String {
    let answer = get_type(router, merged_api_id, type_name, state::SDL_FORMAT).await;
    assert_eq!(
        answer.status, 200,
        "case {label:?}, type {type_name:?}: expected the merged API to hold the composed type, \
         got {} {:?}",
        answer.status, answer.body
    );
    answer.body["type"]["definition"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Every type name the API `api_id` holds, read through `ListTypes` over every page.
async fn type_names(router: &MultiServiceRouter, label: &str, api_id: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut next_token: Option<String> = None;
    loop {
        let mut query = format!("format={}", state::SDL_FORMAT);
        if let Some(token) = next_token.as_deref() {
            query.push_str(&format!("&nextToken={token}"));
        }
        let answer = call(
            router,
            "GET",
            &format!("/v1/apis/{api_id}/types?{query}"),
            None,
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {label:?}: expected ListTypes 200 on {api_id}, got {} {:?}",
            answer.status, answer.body
        );
        for listed in answer.body["types"].as_array().cloned().unwrap_or_default() {
            names.push(listed["name"].as_str().unwrap_or_default().to_string());
        }
        match answer.body["nextToken"].as_str() {
            Some(token) => next_token = Some(token.to_string()),
            None => break,
        }
    }
    names.sort();
    names
}

#[tokio::test]
async fn a_merged_api_composes_the_schemas_of_every_source_api_associated_with_it() {
    let cases = compositions();
    // Every row's fixture is written first and waited for afterwards, so the spans the
    // clock owes the whole table run alongside each other.
    let mut fixtures: Vec<Fixture> = Vec::new();
    for case in &cases {
        fixtures.push(scheduled_fixture(case.label, case.sources).await);
    }
    for (case, fixture) in cases.iter().zip(&fixtures) {
        await_merges(fixture, case.label).await;
    }

    for (case, fixture) in cases.iter().zip(&fixtures) {
        let router = &fixture.router;
        let merged_api_id = &fixture.merged_api_id;

        if case.remerge {
            for association in &fixture.associations {
                let merged = call(
                    router,
                    "POST",
                    &merge_path(merged_api_id, &association.association_id),
                    None,
                )
                .await;
                assert_eq!(
                    merged.status, 200,
                    "case {:?}: expected StartSchemaMerge 200, got {} {:?}",
                    case.label, merged.status, merged.body
                );
            }
            await_merges(fixture, case.label).await;
        }

        for (type_name, members) in case.expected {
            let definition = merged_definition(router, case.label, merged_api_id, type_name).await;
            for member in *members {
                assert_eq!(
                    definition.matches(member).count(),
                    1,
                    "case {:?}, type {type_name:?}: expected the composed definition to carry \
                     {member:?} exactly once, got {definition:?}",
                    case.label
                );
            }
        }

        // Every name the sources declared, and nothing else: a composition holds one type
        // per name, however many source APIs declared it.
        let mut expected_names: Vec<String> = case
            .expected
            .iter()
            .map(|(type_name, _)| (*type_name).to_string())
            .collect();
        expected_names.sort();
        assert_eq!(
            type_names(router, case.label, merged_api_id).await,
            expected_names,
            "case {:?}: expected the merged API to hold exactly the composed names",
            case.label
        );

        // The merge carries the source API's schema INTO the Merged API; it does not write
        // back, so each source still holds exactly what its own document declared.
        for (index, association) in fixture.associations.iter().enumerate() {
            let own = get_type(router, &association.source_api_id, "Query", state::SDL_FORMAT).await;
            assert_eq!(
                own.status, 200,
                "case {:?}, source {index}: expected the source API's own Query, got {} {:?}",
                case.label, own.status, own.body
            );
            let definition = own.body["type"]["definition"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            for (type_name, members) in case.expected {
                if *type_name != "Query" {
                    continue;
                }
                for member in *members {
                    assert_eq!(
                        definition.contains(member),
                        case.sources[index].contains(member),
                        "case {:?}, source {index}: expected the source API's own Query to \
                         declare {member:?} only if its own document did, got {definition:?}",
                        case.label
                    );
                }
            }
        }
    }
}

/// One row of the conflict table: the schema already composed onto the Merged API, the one
/// that cannot be composed with it, and what the two disagree about.
#[derive(Debug)]
struct Conflict {
    label: &'static str,
    /// The source API merged first, whose schema the Merged API holds.
    held: &'static str,
    /// The source API merged second.
    incoming: &'static str,
    /// The `SourceApiAssociationStatus` the second merge must reach.
    status: &'static str,
    /// The type name the detail of a refused merge must name, empty on a row that is no
    /// conflict at all.
    type_name: &'static str,
    /// The member text that type's composed definition must carry, and the one it must
    /// NOT: the second merge is either composed whole or not at all.
    composed: &'static str,
    withheld: &'static str,
}

/// The conflict table: one member declared differently by the two sources, one name
/// declared as two different kinds, one directive written on the same name with different
/// arguments, and — the control row — one member both declare identically, which is no
/// disagreement and composes.
fn conflicts() -> Vec<Conflict> {
    vec![
        Conflict {
            label: "one root field declared with two different types",
            held: "type Query {\n  clash: String\n}\n",
            incoming: "type Query {\n  clash: Int\n}\n",
            status: state::MERGE_FAILED,
            type_name: "Query",
            composed: "clash: String",
            withheld: "clash: Int",
        },
        Conflict {
            label: "one type name declared as two different kinds",
            held: "type Query {\n  token: Token\n}\ntype Token {\n  tokenId: ID!\n}\n",
            incoming: "type Query {\n  tokenTwo: Token\n}\nenum Token {\n  ALPHA\n}\n",
            status: state::MERGE_FAILED,
            type_name: "Token",
            composed: "type Token",
            withheld: "ALPHA",
        },
        Conflict {
            label: "one directive written with two different arguments",
            held: "type Query {\n  marked: Marked\n}\ntype Marked \
                   @aws_cognito_user_pools(cognito_groups: [\"alpha\"]) {\n  markedId: ID!\n}\n",
            incoming: "type Query {\n  markedTwo: Marked\n}\ntype Marked \
                       @aws_cognito_user_pools(cognito_groups: [\"beta\"]) {\n  markedId: ID!\n}\n",
            status: state::MERGE_FAILED,
            type_name: "Marked",
            composed: "alpha",
            withheld: "beta",
        },
        Conflict {
            label: "one root field both declare identically",
            held: "type Query {\n  agreed: String\n}\n",
            incoming: "type Query {\n  agreed: String\n  extra: String\n}\n",
            status: state::MERGE_SUCCESS,
            type_name: "",
            composed: "extra: String",
            withheld: "",
        },
    ]
}

#[tokio::test]
async fn a_declaration_the_merged_schema_cannot_hold_fails_its_own_merge_alone() {
    let cases = conflicts();
    // Written first, waited for afterwards, as the composition table is: each row's two
    // sources are associated in the order they must be merged in, so the row's second
    // merge is the one that arrives at a schema it cannot be combined with.
    let mut fixtures: Vec<Fixture> = Vec::new();
    for case in &cases {
        fixtures.push(scheduled_fixture(case.label, &[case.held, case.incoming]).await);
    }

    for (case, fixture) in cases.iter().zip(&fixtures) {
        let router = &fixture.router;
        let merged_api_id = &fixture.merged_api_id;
        let settled = await_merges(fixture, case.label).await;
        let second = &fixture.associations[1];

        // The merge that ran first is unaffected by the one that could not be composed.
        assert_eq!(
            settled[0]["sourceApiAssociationStatus"].as_str(),
            Some(state::MERGE_SUCCESS),
            "case {:?}: expected the first association to have merged, got {:?}",
            case.label,
            settled[0]
        );

        let association = settled[1].clone();
        assert_eq!(
            association["sourceApiAssociationStatus"].as_str(),
            Some(case.status),
            "case {:?}: expected the second merge to reach {:?}, got {association:?}",
            case.label,
            case.status
        );
        let detail = association["sourceApiAssociationStatusDetail"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        // The detail identifies the merge from the association's own state, and a refused
        // one says what the two schemas disagree about.
        for named in [
            second.source_api_id.as_str(),
            second.association_id.as_str(),
            case.type_name,
        ] {
            assert!(
                detail.contains(named),
                "case {:?}: expected the detail to name {named:?}, got {detail:?}",
                case.label
            );
        }

        let composed = merged_definition(router, case.label, merged_api_id, "Query").await;
        let subject = if case.type_name.is_empty() || case.type_name == "Query" {
            composed
        } else {
            merged_definition(router, case.label, merged_api_id, case.type_name).await
        };
        assert!(
            subject.contains(case.composed),
            "case {:?}: expected the merged definition to carry {:?}, got {subject:?}",
            case.label,
            case.composed
        );
        if !case.withheld.is_empty() {
            assert!(
                !subject.contains(case.withheld),
                "case {:?}: expected the refused declaration {:?} to be absent, got {subject:?}",
                case.label,
                case.withheld
            );
        }
    }
}

#[tokio::test]
async fn a_refused_merge_leaves_the_schema_the_last_performed_merge_carried() {
    let label = "a re-merge that cannot be composed";

    // Two source APIs merged in, each with its own field on the root type.
    let fixture = scheduled_fixture(
        label,
        &[
            "type Query {\n  alpha: String\n}\n",
            "type Query {\n  beta: String\n}\n",
        ],
    )
    .await;
    await_merges(&fixture, label).await;
    let router = &fixture.router;
    let merged_api_id = &fixture.merged_api_id;
    let remerged = &fixture.associations[1];
    let composed = merged_definition(router, label, merged_api_id, "Query").await;
    for member in ["alpha: String", "beta: String"] {
        assert!(
            composed.contains(member),
            "case {label:?}: expected both sources' fields before the refused merge ({member:?}), \
             got {composed:?}"
        );
    }

    // The second source API's schema is replaced with one that cannot be composed: it
    // declares the field the FIRST source declared, with another type.
    let replaced = start_creation(
        router,
        &remerged.source_api_id,
        b"type Query {\n  beta: String\n  alpha: Int\n}\n",
    )
    .await;
    assert_eq!(
        replaced.status, 200,
        "case {label:?}: expected StartSchemaCreation 200, got {} {:?}",
        replaced.status, replaced.body
    );
    await_creation(router, &remerged.source_api_id, label).await;

    let started = call(
        router,
        "POST",
        &merge_path(merged_api_id, &remerged.association_id),
        None,
    )
    .await;
    assert_eq!(
        started.status, 200,
        "case {label:?}: expected StartSchemaMerge 200, got {} {:?}",
        started.status, started.body
    );
    let settled =
        await_terminal_merge(router, label, merged_api_id, &remerged.association_id).await;
    assert_eq!(
        settled["sourceApiAssociationStatus"].as_str(),
        Some(state::MERGE_FAILED),
        "case {label:?}: expected the re-merge to be refused, got {settled:?}"
    );

    // A merge that failed carries no schema: the Merged API still holds exactly what the
    // merges that WERE performed carried, including this association's own last one.
    let kept = merged_definition(router, label, merged_api_id, "Query").await;
    for member in ["alpha: String", "beta: String"] {
        assert!(
            kept.contains(member),
            "case {label:?}: expected the schema the performed merges carried to be kept \
             ({member:?}), got {kept:?}"
        );
    }
    assert!(
        !kept.contains("alpha: Int"),
        "case {label:?}: expected the refused declaration to be absent, got {kept:?}"
    );
}

/// One row of the withdrawal table: how many source APIs are merged in, and from which
/// side the first of them is disassociated.
#[derive(Debug)]
struct Withdrawal {
    label: &'static str,
    /// The schemas of the source APIs merged in, the FIRST of which is disassociated.
    sources: &'static [&'static str],
    /// Whether the deletion is asked for from the MERGED API's side
    /// (`DisassociateSourceGraphqlApi`) rather than from the source API's
    /// (`DisassociateMergedGraphqlApi`): one association, addressed from either of its two
    /// APIs.
    from_the_merged_side: bool,
    /// What the Merged API must hold afterwards: each surviving type name and the member
    /// its definition must still carry.
    left: &'static [(&'static str, &'static str)],
    /// The type names the Merged API must no longer hold at all.
    withdrawn: &'static [&'static str],
}

/// The withdrawal table: the only source API of a Merged API disassociated from either
/// side, and one of two disassociated from either side — where the root type both declared
/// survives with the remaining source's field alone.
fn withdrawals() -> Vec<Withdrawal> {
    vec![
        Withdrawal {
            label: "the only source api, from the source side",
            sources: &[IDENTICAL_ONE],
            from_the_merged_side: false,
            left: &[],
            withdrawn: &["Query", "Widget"],
        },
        Withdrawal {
            label: "the only source api, from the merged side",
            sources: &[IDENTICAL_ONE],
            from_the_merged_side: true,
            left: &[],
            withdrawn: &["Query", "Widget"],
        },
        Withdrawal {
            label: "one of two source apis, from the source side",
            sources: &[SHARED_OBJECT_ONE, QUERY_TWO],
            from_the_merged_side: false,
            left: &[("Query", "greetingTwo")],
            withdrawn: &["Shared"],
        },
        Withdrawal {
            label: "one of two source apis, from the merged side",
            sources: &[SHARED_OBJECT_ONE, QUERY_TWO],
            from_the_merged_side: true,
            left: &[("Query", "greetingTwo")],
            withdrawn: &["Shared"],
        },
    ]
}

/// Read the association until the deletion the clock owes it has been materialised, and
/// answer the refusal the reader then gives.
///
/// A wait on the CONDITION rather than on a fixed time: the transition is one the clock
/// owes, so the association is read again until it is gone or the association harness's own
/// clock-wait budget ([`MERGE_DEADLINE`]) passes — the one deadline every wait of these
/// suites takes, with room for a machine far slower than this one.
async fn await_association_gone(
    router: &MultiServiceRouter,
    label: &str,
    merged_api_id: &str,
    association_id: &str,
) -> Answer {
    let deadline = tokio::time::Instant::now() + MERGE_DEADLINE;
    loop {
        let answer = call(router, "GET", &get_path(merged_api_id, association_id), None).await;
        if answer.status != 200 {
            return answer;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {label:?}: expected the deletion the clock owes to have been materialised, got \
             {answer:?}"
        );
        tokio::time::sleep(POLL_SPACING).await;
    }
}

#[tokio::test]
async fn a_disassociated_source_apis_types_leave_the_merged_api_with_it() {
    let cases = withdrawals();
    // Written first, waited for afterwards, as the composition table is.
    let mut fixtures: Vec<Fixture> = Vec::new();
    for case in &cases {
        fixtures.push(scheduled_fixture(case.label, case.sources).await);
    }
    for (case, fixture) in cases.iter().zip(&fixtures) {
        await_merges(fixture, case.label).await;
    }

    for (case, fixture) in cases.iter().zip(&fixtures) {
        let router = &fixture.router;
        let merged_api_id = &fixture.merged_api_id;
        let merged_api = create_api(
            router,
            &format!("{}-baseline", case.label),
            state::MERGED_API_TYPE,
        )
        .await;
        // The set a Merged API holds before any source was merged in, read rather than
        // written down: what the withdrawal must return this one to.
        let baseline = type_names(router, case.label, &merged_api.id).await;
        let withdrawn = fixture.associations[0].clone();

        // Every type the sources declared is on the Merged API before the deletion, so the
        // absences asserted below are the withdrawal's work.
        for type_name in case.withdrawn {
            merged_definition(router, case.label, merged_api_id, type_name).await;
        }

        let path = if case.from_the_merged_side {
            get_path(merged_api_id, &withdrawn.association_id)
        } else {
            format!(
                "/v1/sourceApis/{}/mergedApiAssociations/{}",
                withdrawn.source_api_id, withdrawn.association_id
            )
        };
        let deleted = call(router, "DELETE", &path, None).await;
        assert_eq!(
            (
                deleted.status,
                deleted.body["sourceApiAssociationStatus"].as_str()
            ),
            (200, Some(state::DELETION_SCHEDULED)),
            "case {:?}: expected the deletion to be scheduled, got {} {:?}",
            case.label,
            deleted.status,
            deleted.body
        );

        let gone = await_association_gone(
            router,
            case.label,
            merged_api_id,
            &withdrawn.association_id,
        )
        .await;
        assert_eq!(
            (
                gone.status,
                gone.error_type.as_str(),
                gone.body["message"].as_str()
            ),
            (
                404,
                "NotFoundException",
                Some(state::ASSOCIATION_NOT_FOUND_MESSAGE)
            ),
            "case {:?}: expected the gone association's own refusal, got {gone:?}",
            case.label
        );

        // The association is gone, so what it contributed is gone with it.
        for type_name in case.withdrawn {
            let answer = get_type(router, merged_api_id, type_name, state::SDL_FORMAT).await;
            assert_eq!(
                (
                    answer.status,
                    answer.error_type.as_str(),
                    answer.body["message"].as_str()
                ),
                (404, "NotFoundException", Some(state::TYPE_NOT_FOUND_MESSAGE)),
                "case {:?}, type {type_name:?}: expected the withdrawn type to be refused, got \
                 {answer:?}",
                case.label
            );
        }

        // What the associations it has LEFT contribute is exactly what it still holds: a
        // name both sources declared keeps the remaining source's member alone.
        for (type_name, member) in case.left {
            let definition = merged_definition(router, case.label, merged_api_id, type_name).await;
            assert!(
                definition.contains(member),
                "case {:?}, type {type_name:?}: expected the remaining source's {member:?}, got \
                 {definition:?}",
                case.label
            );
        }
        let mut expected_names: Vec<String> = baseline.clone();
        expected_names.extend(
            case.left
                .iter()
                .map(|(type_name, _)| (*type_name).to_string()),
        );
        expected_names.sort();
        assert_eq!(
            type_names(router, case.label, merged_api_id).await,
            expected_names,
            "case {:?}: expected the merged API's type set to be the composition of the \
             associations it has left",
            case.label
        );
    }
}
