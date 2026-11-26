// SPDX-License-Identifier: CC0-1.0

//! `pyspark.catalog`: the DATA CATALOG a Spark program's tables are registered in.
//!
//! A Spark program that writes a TABLE rather than a path — `saveAsTable`, and everything a
//! session resolves a table name through — talks to a metastore: the catalog that holds, per
//! table, the columns it has, the partition keys it is laid out by and the location its data
//! lives at. On Amazon Web Services that metastore is the AWS Glue Data Catalog, which a Spark
//! driver reaches through the catalog's own API as the identity the work runs as.
//!
//! So that is what this module is: the metastore client of this runtime, making real Glue calls
//! over the AWS SDK the runtime supplies the guest (`boto3`) — the same transport and the same
//! identity every other call a program makes takes. Which means:
//!
//! * a table a program registers is a table the Data Catalog really holds, which a later
//!   `GetTable` — or Amazon Athena over the same catalog — really reads;
//! * the catalog AUTHORIZES each of those calls against the policies in force on the identity
//!   the program runs as, exactly as it authorizes any other client's, so a program whose role
//!   may not create a table in that catalog FAILS with the catalog's own refusal rather than
//!   silently writing data nothing can find;
//! * and what the driver then reports is that refusal: the exception raised here is of the class
//!   the catalog named ([`CATALOG_SOURCE`]'s error classes), carrying the catalog's own message,
//!   so the traceback in the driver's log names the failure class a client branches on.
//!
//! The DATA a table holds is not the catalog's: it is the objects under the table's location,
//! written through the filesystem every other write of this runtime goes through
//! ([`super::filesystem`]). This module holds the metadata half alone.

/// The source of `pyspark/catalog.py`.
pub(super) const CATALOG_SOURCE: &str = r#"
"""The AWS Glue Data Catalog a Spark session's tables are registered in."""

# The database a table name that names none is resolved in: the database Hive and Spark give a
# session that has set none, which `spark.catalog.currentDatabase()` reports.
DEFAULT_DATABASE = "default"

# The property a Spark session's own warehouse location is configured under, which is where a
# table is laid out when neither the write nor its database names a location.
WAREHOUSE_PROPERTY = "spark.sql.warehouse.dir"

# The suffix Spark's warehouse layout gives the directory of one database.
DATABASE_DIRECTORY_SUFFIX = ".db"


class CatalogException(Exception):
    """A Data Catalog call the catalog itself refused or could not answer.

    Its CLASS is the error the catalog named, and its message the catalog's own: the subclasses
    below are the catalog's error codes, so a driver's traceback names the class a client
    branches on (an authorization refusal apart from a missing entity apart from a conflict).
    """

    def __init__(self, message, code="", operation=""):
        self.code = code
        self.operation = operation
        Exception.__init__(self, message)


class AccessDeniedException(CatalogException):
    """Access to a catalog resource was denied to the identity the call was signed as."""


class EntityNotFoundException(CatalogException):
    """The catalog holds no such entity."""


class AlreadyExistsException(CatalogException):
    """The entity the call would create is already there."""


class InvalidInputException(CatalogException):
    """The call carried input the catalog refused."""


class ConcurrentModificationException(CatalogException):
    """Two writes of the same entity collided."""


class GlueEncryptionException(CatalogException):
    """The catalog could not encrypt or decrypt the entity."""


class ResourceNumberLimitExceededException(CatalogException):
    """A numerical limit of the catalog was exceeded."""


class OperationTimeoutException(CatalogException):
    """The catalog did not answer the call in time."""


# The class each error code the catalog answers with is raised as. A code outside this table is
# raised as the base class carrying that code, so an error this runtime has no class for is
# still reported as what the catalog said rather than as something else.
ERROR_CLASSES = {
    "AccessDeniedException": AccessDeniedException,
    "AlreadyExistsException": AlreadyExistsException,
    "ConcurrentModificationException": ConcurrentModificationException,
    "EntityNotFoundException": EntityNotFoundException,
    "GlueEncryptionException": GlueEncryptionException,
    "InvalidInputException": InvalidInputException,
    "OperationTimeoutException": OperationTimeoutException,
    "ResourceNumberLimitExceededException": ResourceNumberLimitExceededException,
}


def _client():
    """The Data Catalog client, built through the SDK this runtime supplies the guest.

    Its transport is this emulator's own in-process callback, signed as the identity the work
    runs as, so it resolves no endpoint and opens no connection. Imported HERE and not at module
    scope, the way every other client of this runtime is built: a program that never reaches the
    catalog runs in an environment that supplies no SDK at all.
    """
    import boto3

    return boto3.client("glue")


def _answered(error):
    """The (code, message) the catalog's refusal carried, or ('', text) for anything else."""
    response = getattr(error, "response", None)
    if isinstance(response, dict):
        held = response.get("Error") or {}
        return (str(held.get("Code") or ""), str(held.get("Message") or ""))
    return ("", str(error))


def _raised(error, operation):
    """The failure a refused or failed catalog call raises: its class is the error the catalog
    named and its message the catalog's own."""
    code, message = _answered(error)
    held = ERROR_CLASSES.get(code, CatalogException)
    return held(message or str(error), code, operation)


def resolve(name, current=None):
    """The (database, table) a table name addresses.

    A name spelled `<database>.<table>` names that database; one spelling a table alone is
    resolved in the session's current database, which is `DEFAULT_DATABASE` for a session that
    has set none. Backticks around either part are Spark's quoting and are not part of the name.
    """
    text = str(name).strip()
    database = current or DEFAULT_DATABASE
    table = text
    if "." in text:
        database, _marker, table = text.partition(".")
    return (_unquoted(database), _unquoted(table))


def _unquoted(name):
    """One name with Spark's backtick quoting taken off."""
    held = str(name).strip()
    if len(held) >= 2 and held[:1] == "`" and held[-1:] == "`":
        return held[1:-1]
    return held


def database(name):
    """The database `name`, as the catalog holds it ("Database" of `GetDatabase`)."""
    try:
        return _client().get_database(Name=name)["Database"]
    except Exception as error:
        raise _raised(error, "GetDatabase")


def table(database_name, name):
    """The table `name` of `database_name`, or None when the catalog holds no such table.

    A table the catalog does not hold is not a failure of this read — it is what a write asks
    about before it decides whether it is creating or replacing one — so the catalog's miss is
    answered as None and every other answer travels on.
    """
    try:
        return _client().get_table(DatabaseName=database_name, Name=name)["Table"]
    except Exception as error:
        held = _raised(error, "GetTable")
        if isinstance(held, EntityNotFoundException):
            return None
        raise held


def database_exists(name):
    """Whether the catalog holds the database `name`."""
    try:
        database(name)
        return True
    except EntityNotFoundException:
        return False


def create_table(database_name, table_input):
    """Register `table_input` as a new table of `database_name` (`CreateTable`)."""
    try:
        _client().create_table(DatabaseName=database_name, TableInput=table_input)
    except Exception as error:
        raise _raised(error, "CreateTable")


def update_table(database_name, table_input):
    """Replace the registered definition of a table of `database_name` (`UpdateTable`)."""
    try:
        _client().update_table(DatabaseName=database_name, TableInput=table_input)
    except Exception as error:
        raise _raised(error, "UpdateTable")


def create_partitions(database_name, table_name, partition_inputs):
    """Register `partition_inputs` on a table (`BatchCreatePartition`).

    The catalog takes a bounded list per call (`PartitionInputList`, at most 100 entries), so a
    write of more partitions than that is registered in that many calls.
    """
    client = _client()
    held = list(partition_inputs)
    while held:
        batch, held = held[:MAX_PARTITIONS_PER_CALL], held[MAX_PARTITIONS_PER_CALL:]
        try:
            client.batch_create_partition(
                DatabaseName=database_name,
                TableName=table_name,
                PartitionInputList=batch,
            )
        except Exception as error:
            raise _raised(error, "BatchCreatePartition")


# How many partitions ONE `BatchCreatePartition` registers. Source: the modelled
# `PartitionInputList` bound (len 0..100), which is what one call of that API takes.
MAX_PARTITIONS_PER_CALL = 100


# The Hive storage classes a table's data format is registered under: the input format, the
# output format and the serialization library, and the `classification` a catalog client reads
# the format off. SOURCE: the Hive SerDe each data source maps to, as Spark's own `HiveSerDe`
# table gives them — which is what a table written by Spark and read by another engine over the
# same catalog agrees on. A format outside this table is one this runtime registers no storage
# classes for; it is registered with its classification alone, rather than with classes that
# would not read it.
STORAGE_CLASSES = {
    "parquet": {
        "InputFormat": "org.apache.hadoop.hive.ql.io.parquet.MapredParquetInputFormat",
        "OutputFormat": "org.apache.hadoop.hive.ql.io.parquet.MapredParquetOutputFormat",
        "SerializationLibrary": (
            "org.apache.hadoop.hive.ql.io.parquet.serde.ParquetHiveSerDe"
        ),
    },
    "csv": {
        "InputFormat": "org.apache.hadoop.mapred.TextInputFormat",
        "OutputFormat": "org.apache.hadoop.hive.ql.io.HiveIgnoreKeyTextOutputFormat",
        "SerializationLibrary": "org.apache.hadoop.hive.serde2.lazy.LazySimpleSerDe",
        "Parameters": {"field.delim": ","},
    },
    "json": {
        "InputFormat": "org.apache.hadoop.mapred.TextInputFormat",
        "OutputFormat": "org.apache.hadoop.hive.ql.io.HiveIgnoreKeyTextOutputFormat",
        "SerializationLibrary": "org.apache.hive.hcatalog.data.JsonSerDe",
    },
    "text": {
        "InputFormat": "org.apache.hadoop.mapred.TextInputFormat",
        "OutputFormat": "org.apache.hadoop.hive.ql.io.HiveIgnoreKeyTextOutputFormat",
        "SerializationLibrary": "org.apache.hadoop.hive.serde2.lazy.LazySimpleSerDe",
    },
}

# The type a column whose Spark type names no Hive type is registered as. A dataset column of
# nulls alone has the `void` type in Spark, which carries no values to read as anything else.
UNTYPED_COLUMN = "string"

# The table type a table whose data lives at a location of its own is registered as, and the
# parameter that says so, as Hive records an external table.
EXTERNAL_TABLE = "EXTERNAL_TABLE"


def column_type(declared):
    """The catalog type a column declared as the Spark type `declared` is registered with.

    Spark's own type names are Hive's for every type a dataset of this runtime holds, so the
    declared name travels as it is; a column with no type to read (`void`, and a column nothing
    declared) is registered as `UNTYPED_COLUMN`.
    """
    held = str(declared or "").strip().lower()
    if not held or held == "void" or held == "null":
        return UNTYPED_COLUMN
    return held


def table_definition(
    name, columns, types, partition_columns, location, kind, existing=None
):
    """The `TableInput` one table is registered with.

    `columns` are the table's data columns in order, `types` their Spark types by name,
    `partition_columns` the columns the data is laid out by (which Hive holds as the table's
    partition keys rather than as columns of its files), `location` where that data lives and
    `kind` the format it is written in. `existing` is the registered definition this one
    replaces, whose parameters are carried forward so a rewrite does not drop what another
    client recorded on the table.
    """
    held = dict((existing or {}).get("Parameters") or {})
    held["classification"] = kind
    held["EXTERNAL"] = "TRUE"
    # The provider Spark records a data-source table under, which is what its own reader
    # resolves the table's format through.
    held["spark.sql.sources.provider"] = kind
    descriptor = {
        "Columns": [
            {"Name": column, "Type": column_type(types.get(column))}
            for column in columns
        ],
        "Location": location,
    }
    descriptor.update(STORAGE_CLASSES.get(kind, {}))
    serde = descriptor.pop("SerializationLibrary", None)
    parameters = descriptor.pop("Parameters", None)
    if serde is not None:
        descriptor["SerdeInfo"] = {"SerializationLibrary": serde}
        if parameters:
            descriptor["SerdeInfo"]["Parameters"] = parameters
    return {
        "Name": name,
        "TableType": EXTERNAL_TABLE,
        "Parameters": held,
        "StorageDescriptor": descriptor,
        "PartitionKeys": [
            {"Name": column, "Type": column_type(types.get(column))}
            for column in partition_columns
        ],
    }


def partition_input(values, location, kind, columns, types):
    """The `PartitionInput` one partition of a table is registered with.

    A partition carries one value per partition key, "ordered in the same order as the partition
    keys" (`PartitionInput$Values`), and its own location — the directory of the Hive layout its
    rows were written under.
    """
    descriptor = {
        "Columns": [
            {"Name": column, "Type": column_type(types.get(column))}
            for column in columns
        ],
        "Location": location,
    }
    descriptor.update(STORAGE_CLASSES.get(kind, {}))
    serde = descriptor.pop("SerializationLibrary", None)
    parameters = descriptor.pop("Parameters", None)
    if serde is not None:
        descriptor["SerdeInfo"] = {"SerializationLibrary": serde}
        if parameters:
            descriptor["SerdeInfo"]["Parameters"] = parameters
    return {"Values": [str(value) for value in values], "StorageDescriptor": descriptor}


def table_location(option_path, existing, database_record, warehouse, database_name, name):
    """Where the data of the table being written LIVES, or None when nothing names a location.

    The order is Spark's own: the location the write itself names (`path`), else the location
    the registered table already has, else the database's own `LocationUri` with the table's
    name under it, else the session's warehouse directory with the database's and the table's
    under it. A write that resolves none of these has nowhere to put the table's data, which is
    what the caller is told instead of writing it somewhere nobody named.
    """
    if option_path:
        return str(option_path).rstrip("/")
    held = ((existing or {}).get("StorageDescriptor") or {}).get("Location")
    if held:
        return str(held).rstrip("/")
    database_location = (database_record or {}).get("LocationUri")
    if database_location:
        return "%s/%s" % (str(database_location).rstrip("/"), name)
    if warehouse:
        return "%s/%s%s/%s" % (
            str(warehouse).rstrip("/"),
            database_name,
            DATABASE_DIRECTORY_SUFFIX,
            name,
        )
    return None


class Catalog(object):
    """`spark.catalog`: the catalog interface a session hands out.

    Every question it answers is answered by the Data Catalog itself, so what a program reads
    here is what the account's catalog really holds.
    """

    def __init__(self, session):
        self._session = session
        self._current = DEFAULT_DATABASE

    def currentDatabase(self):
        return self._current

    def setCurrentDatabase(self, dbName):
        name = _unquoted(dbName)
        # "Sets the current default database in this session": a database the catalog does not
        # hold cannot become the session's current one, which is the catalog's own answer.
        database(name)
        self._current = name
        return None

    def databaseExists(self, dbName):
        return database_exists(_unquoted(dbName))

    def tableExists(self, tableName, dbName=None):
        database_name, name = resolve(tableName, dbName or self._current)
        return table(database_name, name) is not None
"#;
