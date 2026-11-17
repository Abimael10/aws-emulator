// SPDX-License-Identifier: CC0-1.0

//! Why a statement naming a relation NOBODY registered fails on the RELATION.
//!
//! A statement that qualifies no database resolves its names in
//! [`crate::state::DEFAULT_QUERY_DATABASE`], and that database is one the Data Catalog
//! itself HOLDS for every account ([`glue_state::CATALOG_DEFAULT_DATABASE`], written into
//! the catalog when the store is materialised) rather than one a `CreateDatabase` wrote.
//! So [`super::resolve`] finds the database and then finds that it holds no such relation,
//! which is the table-not-found class naming `catalog.database.table`
//! ([`super::Failure::table_not_found`]) — not the schema-not-found class naming the
//! database.
//!
//! The two names have to be the SAME name for that to hold, which is what the check below
//! pins: Athena's query layer resolving in a database the catalog does not hold would
//! report every unregistered relation as a schema that is not there.

#[cfg(test)]
mod tests {
    use ::service_com_amazonaws_glue_awsglue::state as glue_state;

    /// The database an unqualified name resolves in is the one the Data Catalog holds.
    #[test]
    fn the_default_query_database_is_the_one_the_data_catalog_holds() {
        assert_eq!(
            crate::state::DEFAULT_QUERY_DATABASE,
            glue_state::CATALOG_DEFAULT_DATABASE,
            "a query with no context resolves in {:?}, which the Data Catalog does not \
             hold: it holds {:?}",
            crate::state::DEFAULT_QUERY_DATABASE,
            glue_state::CATALOG_DEFAULT_DATABASE
        );
    }
}
