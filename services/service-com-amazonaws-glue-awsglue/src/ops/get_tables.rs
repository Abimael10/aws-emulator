// SPDX-License-Identifier: CC0-1.0

//! @op GetTables
//!
//! This is the ONLY file to touch for this operation.
//!
//! Lists the tables actually stored in one database, in folded-name order.
//!
//! The tables of a FEDERATED database are not among them: that database "points to an
//! entity outside the Glue Data Catalog", so its tables are held by the source its
//! registration names, and no such source can be reached from here — which is
//! `FederationSourceRetryableException`, exactly as it is for the single-table read
//! (`crate::state::federated_source_refusal`).
//!
//! `Expression` is "A regular expression pattern. If present, only those tables whose
//! names match the pattern are returned" (`GetTablesRequest$Expression`), and it selects
//! the tables whose WHOLE name matches it: a metacharacter keeps its regular-expression
//! meaning (`ev_.*` selects `ev_one` and `ev_two`, `ev?` selects `ev`), while `*` stands
//! for any run of characters (including none), so `events*` selects `events` and
//! `events_raw` and a bare `*` selects everything — unless that `*` is escaped (`\*`) or
//! inside a character class (`[*]`), where it is the expression's own star. That pattern
//! language is [`crate::state::compile_name_pattern`], which every listing of these
//! tables shares (Athena's `ListTableMetadata` over this same catalog is the other), so
//! two listings of one set of tables can never disagree; the `*` substitution is what
//! keeps it Hive-metastore compatible, the model folding both `DatabaseInput$Name` and
//! `TableInput$Name` "for Hive compatibility". The match is case-SENSITIVE, so a pattern
//! spelled in another case than the stored (folded) name selects nothing. A value that is
//! no pattern at all (an unclosed group, a quantifier with nothing to repeat) is refused
//! with `InvalidInputException`, as is one whose compiled matcher would be larger than
//! [`MAX_PATTERN_PROGRAM_BYTES`], so the work a caller-supplied pattern can buy stays
//! bounded. `MaxResults` bounds the tables ONE response examines, in the order the catalog
//! keeps them in, and the page holds those of them the request selects — so a FILTERED
//! listing serves pages shorter than the size it asked for (an empty one included) and goes
//! on being continued while stored tables are left. The returned `NextToken` is an OPAQUE
//! token this store minted for THIS listing ([`crate::state::pagination`]) — never the
//! name of a table, which would put catalog contents into a cursor and let a client
//! assemble a position the catalog never offered — so the next call resumes at the first
//! stored name strictly after the one the token carries, and a `NextToken` this store did
//! not mint for this catalog and database (a forged one, one minted for another database,
//! one from another process) names no position and is refused with `InvalidInputException`.
//! A page size ABOVE the largest page the service admits at that member
//! ([`MAX_PAGE_SIZE`]) is refused at the request boundary like any value outside a
//! member's range, as a `ValidationException` naming the member and the bound; a size
//! between the modelled maximum and that bound is served, so one call asking for a
//! thousand tables answers with every matching table and no `NextToken`. A size below the
//! modelled minimum names no servable page and is refused with this service's own
//! `InvalidInputException`, the error class a Glue client can match.
//!
//! Members this slice does not act on, and why: `AuditContext` is input-only Lake
//! Formation audit metadata that does not change this operation's answer. Every other
//! member IS acted on: `Expression`, `MaxResults` and `NextToken` as above;
//! `AttributesToGet` projects each returned table (and its documented validation is
//! enforced) — it names which attributes of the table DEFINITION come back, so what the
//! catalog answers about the element it is serving (the catalog and database that hold the
//! table, and the flags the catalog answers definitely) travels with every entry whatever
//! the projection ([`Projection::apply`]); `ResourceShareType` selects federated
//! tables only, a stored table being
//! federated exactly when it carries a `FederatedTable`; and `TransactionId`,
//! `QueryAsOfTime` and `IncludeStatusDetails` are REFUSED, because serving the latest
//! unqualified state for a point-in-time, transactional or view-status read would be a
//! wrong-but-live answer.
//!
//! `DatabaseName` supplied as null or empty, `ResourceShareType` and the entries of
//! `AttributesToGet` are judged outside the framework's constraint walk: the name by
//! [`crate::service`]'s pre-handler wrapper (an absent required member never reaches a
//! handler) and the two enumerated members by this handler
//! ([`crate::service::resource_share_type_not_valid`],
//! [`crate::service::ATTRIBUTE_NOT_VALID_TO_GET`]), so each earns the catalog's own
//! `InvalidInputException` and its own sentence.
//!
//! ## Shape of an operation
//!
//! Each operation is a struct that owns the shared engine and implements
//! [`smithy_server_runtime::Operation`]. The framework constructs one per service via
//! `GetTables::new(engine)` and drives it through the centralized request walk:
//! deserialize → validate → `handle` → serialize. You write ONLY the `handle` body.
//!
//! ## Reaching this service's state
//!
//! The engine is held on `self` (constructor-injected), NOT passed per request — so
//! engine access is guaranteed by the time any request runs. Reach the store through
//! [`crate::state::service_state`] and read and write it through the guard it hands back:
//!
//! ```ignore
//!     let state = crate::state::service_state(&*self.engine)
//!         .ok_or(/* a modelled server error */)?;
//!     state.lock().your_field = /* ... */;
//! ```
//!
//! `ctx` ([`smithy_server_runtime::OperationContext`]) carries request metadata only —
//! headers, URI, method, and anything middleware placed in `ctx.parts().extensions`.
//! The engine is NOT reached through `ctx`.
//!
//! @generated by the emulator core scaffold op-stub generator.
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `GetTables`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json")
//! ```
//!
//! Retrieves the definitions of some or all of the tables in a given Database.
//!
//! ## Input
//!
//! * `AttributesToGet` (TableAttributesList) — Specifies the table fields returned by the
//!   GetTables call. This parameter doesn’t accept an empty list. The request must include
//!   NAME. The following are the valid combinations of values: NAME - Names of all tables in
//!   the database. NAME, TABLE_TYPE - Names of all tables and the table types.
//! * `AuditContext` (AuditContext) — A structure containing the Lake Formation audit context.
//! * `CatalogId` (CatalogIdString, len 1..255, pattern
//!   ^[\u0020-\uD7FF\uE000-\uFFFD\uD800\uDC00-\uDBFF\uDFFF\t]*$) — The ID of the Data Catalog
//!   where the tables reside. If none is provided, the Amazon Web Services account ID is used
//!   by default.
//! * `DatabaseName` (NameString, len 1..255, pattern
//!   ^[\u0020-\uD7FF\uE000-\uFFFD\uD800\uDC00-\uDBFF\uDFFF\t]*$) **required** — The database
//!   in the catalog whose tables to list. For Hive compatibility, this name is entirely
//!   lowercase.
//! * `Expression` (FilterString, len 0..2048, pattern
//!   ^[\u0020-\uD7FF\uE000-\uFFFD\uD800\uDC00-\uDBFF\uDFFF\t]*$) — A regular expression
//!   pattern. If present, only those tables whose names match the pattern are returned.
//! * `IncludeStatusDetails` (BooleanNullable) — Specifies whether to include status details
//!   related to a request to create or update an Glue Data Catalog view.
//! * `MaxResults` (CatalogGetterPageSize, range 1..100) — The maximum number of tables to
//!   return in a single response.
//! * `NextToken` (Token) — A continuation token, included if this is a continuation call.
//! * `QueryAsOfTime` (Timestamp) — The time as of when to read the table contents. If not
//!   set, the most recent transaction commit time will be used. Cannot be specified along
//!   with TransactionId.
//! * `ResourceShareType` (TableResourceShareType, one of: ALL | FEDERATED) — Specifies which
//!   tables the GetTables call returns. The allowable values are FEDERATED or ALL. If set to
//!   FEDERATED, returns only federated tables, which reference an entity outside the Glue
//!   Data Catalog. If set to ALL, returns all tables in the database, both federated and
//!   non-federated.
//! * `TransactionId` (TransactionIdString, len 1..255, pattern ^[\p{L}\p{N}\p{P}]*$) — The
//!   transaction ID at which to read the table contents.
//!
//! ## Output
//!
//! * `NextToken` (Token) — A continuation token, present if the current list segment is not
//!   the last.
//! * `TableList` (TableList) — A list of the requested Table objects.
//!
//! ## Errors
//!
//! * `EntityNotFoundException` (HTTP 400) — A specified entity does not exist
//! * `FederationSourceException` (HTTP 400) — A federation source failed.
//! * `FederationSourceRetryableException` (HTTP 400) — A federation source failed, but the
//!   operation may be retried.
//! * `GlueEncryptionException` (HTTP 400) — An encryption operation failed.
//! * `InternalServiceException` (HTTP 500) — An internal service error occurred.
//! * `InvalidInputException` (HTTP 400) — The input provided was not valid.
//! * `OperationTimeoutException` (HTTP 400) — The operation timed out.
//! * `AccessDeniedException` (HTTP 400) — widened in through `extra_errors` (see below):
//!   "Access to a resource was denied." A `CatalogId` naming another account's Data
//!   Catalog is reachable only through that catalog's resource-based policy, so a caller
//!   with no grant on it is refused here before any database is looked up.

#[allow(unused_imports)]
use super::types::*;

/// The page this operation serves when the request names no `MaxResults`. Source: the
/// model's `MaxResults` target `CatalogGetterPageSize`, whose range is 1..100, so the
/// largest page the model describes is the one a request that asks for no particular size
/// is served.
///
/// A request that DOES name a size above this one is served that size, up to
/// [`MAX_PAGE_SIZE`]: the service answers a `MaxResults` of a thousand with every matching
/// table on one page, so the modelled maximum bounds the default and not the request.
const DEFAULT_PAGE_SIZE: i32 = 100;

/// The largest page a request may ask for. RECORDED behaviour: the live service refuses a
/// `MaxResults` of 1001 at this member with the bound 1000, so a thousand is the page size
/// it admits — above the 100 the checked-in model declares for `CatalogGetterPageSize`,
/// which is why a size between the two is served rather than refused.
const MAX_PAGE_SIZE: i32 = 1_000;

/// How the API spells the member a page size is sent at, for the refusal of a size outside
/// its range: the member's own name as a parameter path, which is the lowerCamel spelling
/// the validating walk builds for `MaxResults`
/// (`aws_smithy_schema_server::constraints::path::member`).
const MAX_RESULTS_MEMBER: &str = "maxResults";

/// The smallest page a request may ask for: the minimum of that same modelled range. A
/// request naming fewer than one table per page asks for a page no listing can serve, and
/// is refused with this service's own `InvalidInputException` — never with the
/// cross-service `ValidationException`, which no Glue error class matches (`MaxResults`
/// carries `skip_validation`, see `super::types::get_tables_types`).
const MIN_PAGE_SIZE: i32 = 1;

/// Ceiling on the size of the matcher one `Expression` compiles to, re-exported from
/// where it lives beside the pattern language it bounds
/// ([`crate::state::MAX_PATTERN_PROGRAM_BYTES`]) so every listing of these tables is
/// bounded by ONE number. A pattern past it is refused here with `InvalidInputException`.
pub use crate::state::MAX_PATTERN_PROGRAM_BYTES;

/// Which members of each `Table` the request asked for.
///
/// From `AttributesToGet`, whose documented valid combinations for this operation are
/// exactly `NAME` and `NAME, TABLE_TYPE`; absent, the whole `Table` is returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Projection {
    Full,
    Name,
    NameAndTableType,
}

impl Projection {
    /// Narrow a rebuilt `Table` to the members this projection asked for.
    ///
    /// `AttributesToGet` names ATTRIBUTES of the table definition, so a projection narrows
    /// the definition — and not what the catalog answers ABOUT the element it is serving:
    /// which catalog and database hold the table, whether a row-level filter is in force
    /// and whether the object is a materialized view travel on every element the catalog
    /// returns, which is what a client keying a listing by database, branching on row
    /// filtering or telling a table from a materialized view reads. `IsMultiDialectView`,
    /// by contrast, describes the view DEFINITION — which dialects its query can be read
    /// in — so it is projected away with the rest of the definition.
    fn apply(self, table: Table) -> Table {
        let identified = |projected: Table| Table {
            catalog_id: table.catalog_id.clone(),
            database_name: table.database_name.clone(),
            database_id: table.database_id.clone(),
            is_row_filtering_enabled: table.is_row_filtering_enabled,
            is_materialized_view: table.is_materialized_view,
            ..projected
        };
        match self {
            Self::Full => table,
            Self::Name => identified(Table {
                name: table.name.clone(),
                ..Table::default()
            }),
            Self::NameAndTableType => identified(Table {
                name: table.name.clone(),
                table_type: table.table_type.clone(),
                ..Table::default()
            }),
        }
    }
}

/// The scope this listing's continuation tokens are bound to: the set being paged is the
/// tables of ONE database of ONE Data Catalog, so a token minted while listing it resolves
/// against nothing else ([`crate::state::listing_scope`]).
///
/// The database is named as the catalog KEYS it — the Hive-compatible fold every catalog
/// name is stored and looked up under — so a continuation spelled in another case resumes
/// the listing it continues, exactly as it addresses the same database.
fn page_token_scope(operation: &str, catalog_id: &str, database_name: &str) -> String {
    crate::state::listing_scope(
        operation,
        &[catalog_id, &crate::state::fold_name(database_name)],
    )
}

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `GetTablesError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-glue-awsglue.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `GetTablesError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
//
// `AccessDeniedException` — "Access to a resource was denied." — is widened in through
// `extra_errors`: this SERVICE models the shape, but this operation's own `errors` list
// omits it, and a cross-account read of a Data Catalog the caller holds no grant on is
// refused as unauthorized rather than as a missing entity.
#[smithy_macros::operation_error(
    "com.amazonaws.glue#GetTables",
    extra_errors = ["AccessDeniedException"]
)]
#[derive(Debug)]
pub enum GetTablesErrorModeled {
    AccessDeniedException(AccessDeniedException),
    EntityNotFoundException(EntityNotFoundException),
    FederationSourceException(FederationSourceException),
    FederationSourceRetryableException(FederationSourceRetryableException),
    GlueEncryptionException(GlueEncryptionException),
    InternalServiceException(InternalServiceException),
    InvalidInputException(InvalidInputException),
    OperationTimeoutException(OperationTimeoutException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::get_tables::GetTablesError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type GetTablesError = crate::scaffolded::Scaffolded<GetTablesErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<GetTablesErrorModeled> for GetTablesError {
    fn from(modeled: GetTablesErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `GetTables` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct GetTables<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> GetTables<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for GetTables<Eng>
{
    type Input = GetTablesInput;
    type Output = GetTablesOutput;
    type Error = GetTablesError;

    const NAME: &'static str = "GetTables";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_TABLES_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_TABLES_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        // A `MaxResults` outside the range this member admits is refused where the
        // validating walk would have refused it: BEFORE any other member is read, as the
        // cross-service `ValidationException` naming the member and the bound. The member
        // carries `skip_validation` (the modelled range is narrower than the service's own
        // bound), so this operation applies the range the service enforces.
        if let Some(requested) = input.max_results {
            if requested > MAX_PAGE_SIZE {
                return Err(crate::scaffolded::Scaffolded::validation(
                    crate::state::messages::page_size_above_maximum(
                        MAX_RESULTS_MEMBER,
                        requested,
                        MAX_PAGE_SIZE,
                    ),
                ));
            }
        }
        // The model documents that `QueryAsOfTime` "cannot be specified along with
        // TransactionId", so that pair is a client error whatever else is supported.
        if input.transaction_id.is_some() && input.query_as_of_time.is_some() {
            return Err(GetTablesErrorModeled::InvalidInputException(
                InvalidInputException {
                    message: Some(
                        "QueryAsOfTime cannot be specified along with TransactionId".into(),
                    ),
                    from_federation_source: None,
                },
            )
            .into());
        }
        // Governed tables and point-in-time reads need a transaction log and a table
        // version history this emulator does not keep, so a request that asks for one
        // is answered "not written yet" rather than silently served from the latest
        // state.
        if input.transaction_id.is_some() || input.query_as_of_time.is_some() {
            return Err(crate::scaffolded::Scaffolded::not_implemented(
                "GetTables with TransactionId or QueryAsOfTime",
            ));
        }
        // `IncludeStatusDetails` asks for the status of a Data Catalog VIEW create or
        // update, which this slice does not model, so ASKING for it is refused; not
        // asking for it is served normally.
        if input.include_status_details == Some(true) {
            return Err(crate::scaffolded::Scaffolded::not_implemented(
                "GetTables with IncludeStatusDetails",
            ));
        }

        // `AttributesToGet` projects each returned table. The model: it "doesn't accept
        // an empty list", "must include NAME", and its valid combinations are `NAME`
        // and `NAME, TABLE_TYPE`.
        let projection = match input.attributes_to_get.as_deref() {
            None => Projection::Full,
            Some([]) => {
                return Err(GetTablesErrorModeled::InvalidInputException(
                    InvalidInputException {
                        message: Some(
                            crate::service::ATTRIBUTES_TO_GET_CANNOT_BE_EMPTY.to_string(),
                        ),
                        from_federation_source: None,
                    },
                )
                .into())
            }
            Some(attributes) => {
                // An entry that is no member of the enum this list targets
                // (`TableAttributes`) is judged here rather than at the framework boundary
                // (`skip_validation` on `GetTablesRequest`), so it earns the catalog's own
                // sentence for an attribute it does not get. Which values the enum admits
                // comes from the generated, model-derived value set.
                if crate::service::attribute_outside_set(attributes, TABLE_ATTRIBUTES_VALUES) {
                    return Err(GetTablesErrorModeled::InvalidInputException(
                        InvalidInputException {
                            message: Some(
                                crate::service::ATTRIBUTE_NOT_VALID_TO_GET.to_string(),
                            ),
                            from_federation_source: None,
                        },
                    )
                    .into());
                }
                let wanted: Vec<&str> = attributes.iter().map(String::as_str).collect();
                // A list that does not name `NAME` asks for a projection the operation
                // never serves — "[t]he request must include the NAME" — and the catalog
                // refuses it with ONE fixed sentence that echoes nothing of the request
                // (`crate::service::ATTRIBUTES_TO_GET_MUST_CONTAIN_NAME`).
                if !wanted.contains(&"NAME") {
                    return Err(GetTablesErrorModeled::InvalidInputException(
                        InvalidInputException {
                            message: Some(
                                crate::service::ATTRIBUTES_TO_GET_MUST_CONTAIN_NAME.to_string(),
                            ),
                            from_federation_source: None,
                        },
                    )
                    .into());
                }
                match wanted.as_slice() {
                    ["NAME"] => Projection::Name,
                    ["NAME", "TABLE_TYPE"] | ["TABLE_TYPE", "NAME"] => {
                        Projection::NameAndTableType
                    }
                    _ => {
                        return Err(GetTablesErrorModeled::InvalidInputException(
                            InvalidInputException {
                                message: Some(format!(
                                    "AttributesToGet names NAME, but the valid combinations \
                                     are NAME and NAME,TABLE_TYPE, not {wanted:?}"
                                )),
                                from_federation_source: None,
                            },
                        )
                        .into())
                    }
                }
            }
        };

        // `ResourceShareType`: "If set to FEDERATED, returns only federated tables,
        // which reference an entity outside the Glue Data Catalog. If set to ALL,
        // returns all tables in the database, both federated and non-federated.\" A
        // stored table is federated exactly when it carries a `FederatedTable`.
        let federated_only = match input.resource_share_type.as_deref() {
            None | Some("ALL") => false,
            Some("FEDERATED") => true,
            // A share type outside the enum this member targets
            // (`TableResourceShareType`) is judged here rather than at the framework
            // boundary (`skip_validation` on `GetTablesRequest`), so it earns the
            // catalog's own sentence naming the value the request sent.
            Some(other) => {
                return Err(GetTablesErrorModeled::InvalidInputException(
                    InvalidInputException {
                        message: Some(crate::service::resource_share_type_not_valid(other)),
                        from_federation_source: None,
                    },
                )
                .into())
            }
        };
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(GetTablesErrorModeled::InternalServiceException(
                InternalServiceException {
                    message: Some("the Glue Data Catalog store is unavailable".into()),
                },
            )
            .into());
        };

        let catalog_id = crate::state::resolve_catalog_id(input.catalog_id.as_ref());
        let region = crate::state::request_region(ctx.parts());
        // Whether the caller may read this catalog at all is decided BEFORE any database
        // is looked up: a `CatalogId` of another account is reachable only through that
        // catalog's own resource policy (see `crate::state::catalog_refusal`).
        let catalog_refusal =
            crate::state::catalog_refusal(&state.lock(), &catalog_id, &region, Self::NAME);
        match catalog_refusal {
            None => {}
            Some(crate::state::CatalogRefusal::NotFound(message)) => {
                return Err(GetTablesErrorModeled::EntityNotFoundException(
                    EntityNotFoundException {
                        message: Some(message),
                        from_federation_source: None,
                    },
                )
                .into())
            }
            Some(crate::state::CatalogRefusal::NotAuthorized(message)) => {
                return Err(GetTablesErrorModeled::AccessDeniedException(
                    AccessDeniedException {
                        message: Some(message),
                    },
                )
                .into())
            }
        }

        // The scope every continuation token of THIS listing is bound to, built once from
        // the catalog and database the request addressed.
        let token_scope = page_token_scope(Self::NAME, &catalog_id, &input.database_name);

        // `Expression` is a regular expression in which `*` stands for any run of
        // characters, matched against the WHOLE table name through the ONE pattern
        // language every listing of these tables shares
        // ([`crate::state::compile_name_pattern`]). A value that is no pattern at all, or
        // whose matcher would be larger than that language compiles, is refused rather
        // than silently selecting nothing.
        let filter = match input.expression.as_deref() {
            None => None,
            Some(pattern) => match crate::state::compile_name_pattern(pattern) {
                Ok(matcher) => Some(matcher),
                // The refusal relays what the engine compiling the expression said about
                // it — the fault, the index into the CALLER's own expression and the caret
                // under it — and, for a value that carries no such fault, states that this
                // catalog can build no matcher for it.
                Err(_) => {
                    let message = crate::state::messages::expression_syntax_fault(pattern)
                        .unwrap_or_else(|| {
                            crate::state::messages::expression_not_matchable(pattern)
                        });
                    return Err(GetTablesErrorModeled::InvalidInputException(
                        InvalidInputException {
                            message: Some(message),
                            from_federation_source: None,
                        },
                    )
                    .into());
                }
            },
        };

        // The page size: what the request asked for, else the page a request that names no
        // size is served. A size below the modelled minimum names no servable page and is
        // refused as this service's own input error; a size between the modelled maximum
        // and [`MAX_PAGE_SIZE`] is served, because the service serves it.
        let requested_page_size = input.max_results.unwrap_or(DEFAULT_PAGE_SIZE);
        if requested_page_size < MIN_PAGE_SIZE {
            return Err(GetTablesErrorModeled::InvalidInputException(
                InvalidInputException {
                    message: Some(
                        crate::state::messages::PAGE_SIZE_NOT_POSITIVE.to_string(),
                    ),
                    from_federation_source: None,
                },
            )
            .into());
        }
        let page_size = usize::try_from(requested_page_size).unwrap_or(usize::MAX);

        // What the caller holds in this Data Catalog, asked BEFORE the store is locked:
        // Lake Formation reads this same Data Catalog to resolve the resources its grants
        // name. A table the caller holds no permission on is left OUT of the listing, just
        // as a database it may not describe is left out of `GetDatabases`; `None` is Lake
        // Formation saying nothing, under which every table is listed.
        let permissions = crate::state::lake_formation::catalog_permissions(
            &*self.engine,
            ctx,
            &region,
            &catalog_id,
        )
        .await;

        // The page is built under the store's guard and the guard released before KMS is
        // asked anything: the key material of the tables on the page travels out with it.
        let (table_list, next_token, encryptions) = {
            let store = state.lock();
            let Some(database) = store.database(&catalog_id, &input.database_name) else {
                return Err(GetTablesErrorModeled::EntityNotFoundException(
                    EntityNotFoundException {
                        message: Some(crate::state::messages::database_not_found(
                            &input.database_name,
                        )),
                        from_federation_source: None,
                    },
                )
                .into());
            };

            // The tables of a FEDERATED database are held by the source that database
            // points at, not by the catalog, so the listing is answered by that source —
            // and no such source can be reached from here
            // (`crate::state::federated_source_refusal`).
            if let Some(message) = crate::state::federated_source_refusal(database) {
                return Err(GetTablesErrorModeled::FederationSourceRetryableException(
                    FederationSourceRetryableException {
                        message: Some(message),
                    },
                )
                .into());
            }

            // The tables of this database in the ONE order the catalog keeps them in,
            // which is the order a page SCANS and a cursor resumes against.
            let ordered: Vec<&crate::state::TableEntry> = database.tables_ordered();

            // The token names the last table the previous page SCANNED, so this page
            // resumes at the first stored name strictly after it — which stays correct
            // across tables created or deleted between two calls. It is opaque and bound to
            // THIS catalog and database ([`crate::state::pagination`]), so a value this
            // store did not mint for this listing — a forged cursor, a cursor minted for
            // another database, one from another process — names no position and is refused
            // rather than resolved against a name that happens to sort somewhere.
            let start = match input.next_token.as_deref() {
                Some(token) => {
                    let Some(cursor) = crate::state::decode_page_token(
                        &store.page_token_key,
                        &token_scope,
                        token,
                    ) else {
                        return Err(GetTablesErrorModeled::InvalidInputException(
                            InvalidInputException {
                                message: Some(
                                    crate::state::messages::INVALID_TOKEN_VALUE.to_string(),
                                ),
                                from_federation_source: None,
                            },
                        )
                        .into());
                    };
                    ordered.partition_point(|table| table.name.as_str() <= cursor.as_str())
                }
                None => 0,
            };
            // `MaxResults` is "the maximum number of tables to return in a single
            // response", and what one response examines is `MaxResults` tables of the
            // database: the page holds those of them the request SELECTS, so a filtered
            // listing serves pages shorter than the size it asked for — an empty one
            // included — and goes on being continued while stored tables are left. Cutting
            // on the MATCHES instead would make a page's content depend on how many
            // non-matching tables the catalog holds between two matches, which is not what
            // a continuation of this listing resumes.
            let scanned_to = start.saturating_add(page_size).min(ordered.len());
            let scanned = &ordered[start..scanned_to];

            let page: Vec<&crate::state::TableEntry> = scanned
                .iter()
                .copied()
                .filter(|table| !federated_only || table.federated_table.is_some())
                .filter(|table| {
                    filter
                        .as_ref()
                        .is_none_or(|matcher| matcher.is_match(&table.name))
                })
                .filter(|table| {
                    permissions.as_ref().is_none_or(|held| {
                        held.may_describe_table(&input.database_name, &table.name)
                    })
                })
                .collect();
            // The listing serves the SAME projection as the single-table read, element by
            // element — including the catalog/database identity and the row-filtering flag
            // — so a client that works from a listing is never less informed than one that
            // re-reads each table. Only `AttributesToGet` narrows an element, and then to
            // the attributes it named BESIDE that identity, which the catalog writes onto
            // every table it returns.
            let table_list: Vec<Table> = page
                .iter()
                .map(|table| {
                    // Each element is narrowed to the columns the caller's grants on THAT
                    // table cover, exactly as the single-table read narrows its answer.
                    let answered = table.to_table(&database.database_id);
                    let answered = match permissions
                        .as_ref()
                        .and_then(|held| {
                            held.table_metadata_scope(&input.database_name, &table.name)
                        }) {
                        Some(scope) => {
                            crate::state::lake_formation::narrowed_table(answered, &scope)
                        }
                        None => answered,
                    };
                    projection.apply(answered)
                })
                .collect();
            // The key material of every table ON THE PAGE: this read answers each of them,
            // so each has to be decrypted before any is served.
            let encryptions: Vec<crate::state::catalog_encryption::ObjectEncryption> = page
                .iter()
                .filter_map(|table| table.encryption.clone())
                .collect();
            // The token names the last table the page SCANNED, taken from the scan itself
            // so there is no index arithmetic to underflow, and minted for this listing so
            // only this store can resolve it. A page that reached the end of the database
            // continues nothing, so it carries none.
            let next_token = if scanned_to < ordered.len() {
                scanned.last().map(|table| {
                    crate::state::encode_page_token(
                        &store.page_token_key,
                        &token_scope,
                        &table.name,
                    )
                })
            } else {
                None
            };
            (table_list, next_token, encryptions)
        };

        // A table written while its catalog encrypted catalog writes under a KMS key is
        // listed only while that key is usable, exactly as the single-table read serves it
        // (`crate::ops::get_table`): the page is answered once KMS has opened the data key
        // of every table on it, and KMS's own refusal is relayed as this operation's
        // `GlueEncryptionException`.
        if let Err(message) =
            crate::state::catalog_encryption::unseal_all(ctx, &region, &encryptions).await
        {
            return Err(GetTablesErrorModeled::GlueEncryptionException(
                GlueEncryptionException {
                    message: Some(message),
                },
            )
            .into());
        }

        Ok(GetTablesOutput {
            table_list: Some(table_list),
            next_token,
        })
    }
}
