// SPDX-License-Identifier: CC0-1.0

//! The configuration CLASSIFICATIONS Amazon EMR applies to a cluster's applications.
//!
//! One concern of this service's state, split out of [`super::launch_shape_refusals`], which
//! holds the rules a launch's configuration tree is judged by: "a configuration
//! classification … specif[ies] the configuration file of an application"
//! (`Configuration$Classification` doc, and the Release Guide's "Configuring applications"
//! page), so the classification a configuration names has to be one Amazon EMR applies at
//! all — a value that names no configuration file is applied by nothing, and is refused with
//! nothing stored.
//!
//! The catalogue below is CATALOG DATA: the classification names the Amazon EMR Release
//! Guide's "Configuring applications" page publishes, one per configuration file of each
//! application Amazon EMR bundles ([`super::APPLICATION_CATALOG`] lists those
//! applications). It is a fact about the service rather than account state, so it is a
//! table here rather than anything a caller can write.

/// Every configuration classification Amazon EMR applies that is a FILE of an application,
/// as the Release Guide publishes them: one per configuration file of a bundled application.
///
/// The bare application names are not listed here — a cluster-wide property of an
/// application is set under the application's own name, which is read off the catalogue of
/// applications Amazon EMR bundles ([`is_bundled_application_name`]).
///
/// CATALOG DATA, from the Amazon EMR Release Guide's "Configuring applications" page. The
/// list is read as a whole ([`is_published_classification`]), so an entry's position in it
/// means nothing; it is grouped by application to be read beside that page.
pub const CONFIGURATION_CLASSIFICATIONS: &[&str] = &[
    // Hadoop, HDFS, YARN and MapReduce.
    "capacity-scheduler",
    "container-executor",
    "container-log4j",
    "core-site",
    "fair-scheduler",
    "hadoop-env",
    "hadoop-kms-acls",
    "hadoop-kms-env",
    "hadoop-kms-log4j",
    "hadoop-kms-site",
    "hadoop-log4j",
    "hadoop-ssl-client",
    "hadoop-ssl-server",
    "hdfs-encryption-zones",
    "hdfs-env",
    "hdfs-site",
    "httpfs-env",
    "httpfs-site",
    "mapred-env",
    "mapred-site",
    "yarn-env",
    "yarn-site",
    // The Amazon EMR file system.
    "emrfs-site",
    // Hive and HCatalog.
    "hcatalog-env",
    "hcatalog-server-connections",
    "hcatalog-webhcat-env",
    "hcatalog-webhcat-log4j2",
    "hcatalog-webhcat-site",
    "hive-beeline-log4j2",
    "hive-env",
    "hive-exec-log4j2",
    "hive-llap-daemon-log4j2",
    "hive-log4j2",
    "hive-parquet-logging",
    "hive-site",
    "hivemetastore-site",
    "hiveserver2-site",
    // HBase and Phoenix.
    "hbase-env",
    "hbase-log4j",
    "hbase-metrics",
    "hbase-policy",
    "hbase-site",
    "phoenix-hbase-metrics",
    "phoenix-hbase-site",
    "phoenix-log4j",
    // Spark.
    "spark-defaults",
    "spark-env",
    "spark-hive-site",
    "spark-log4j",
    "spark-log4j2",
    "spark-metrics",
    // Presto and Trino, including their connectors.
    "presto-config",
    "presto-connector-blackhole",
    "presto-connector-cassandra",
    "presto-connector-hive",
    "presto-connector-jmx",
    "presto-connector-kafka",
    "presto-connector-localfile",
    "presto-connector-memory",
    "presto-connector-mongodb",
    "presto-connector-mysql",
    "presto-connector-postgresql",
    "presto-connector-raptor",
    "presto-connector-redis",
    "presto-connector-redshift",
    "presto-connector-tpcds",
    "presto-connector-tpch",
    "presto-env",
    "presto-log",
    "presto-node",
    "presto-password-authenticator",
    "trino-config",
    "trino-connector-blackhole",
    "trino-connector-cassandra",
    "trino-connector-delta",
    "trino-connector-hive",
    "trino-connector-hudi",
    "trino-connector-iceberg",
    "trino-connector-jmx",
    "trino-connector-kafka",
    "trino-connector-localfile",
    "trino-connector-memory",
    "trino-connector-mongodb",
    "trino-connector-mysql",
    "trino-connector-postgresql",
    "trino-connector-redis",
    "trino-connector-redshift",
    "trino-connector-tpcds",
    "trino-connector-tpch",
    "trino-env",
    "trino-exchange-manager",
    "trino-log",
    "trino-node",
    "trino-password-authenticator",
    // Flink.
    "flink-conf",
    "flink-log4j",
    "flink-log4j-cli",
    "flink-log4j-console",
    "flink-log4j-session",
    // Livy and Zeppelin.
    "livy-conf",
    "livy-env",
    "livy-log4j",
    "livy-log4j2",
    "zeppelin-env",
    "zeppelin-log4j",
    "zeppelin-site",
    // The notebook surfaces.
    "jupyter-hub-conf",
    "jupyter-notebook-conf",
    "jupyter-s3-conf",
    "jupyter-sparkmagic-conf",
    // The table formats.
    "delta-defaults",
    "hudi-defaults",
    "hudi-env",
    "iceberg-defaults",
    // Oozie, Pig, Sqoop, Tez, Hue and ZooKeeper.
    "hue-ini",
    "oozie-env",
    "oozie-log4j",
    "oozie-site",
    "pig-env",
    "pig-log4j",
    "pig-properties",
    "sqoop-env",
    "sqoop-oraoop-site",
    "sqoop-site",
    "tez-env",
    "tez-site",
    "zookeeper-config",
    "zookeeper-log4j",
];

/// Whether `name` is the name of an application Amazon EMR bundles
/// ([`super::APPLICATION_CATALOG`]), which is the classification a CLUSTER-WIDE property of
/// that application is set under.
///
/// "A case-insensitive list of applications for Amazon EMR to install and configure"
/// (`RunJobFlow$Applications` doc) is how a launch names them, and a classification spells
/// the same name in lower case (`spark` for the application `Spark`), so the comparison is
/// case-insensitive here too.
pub fn is_bundled_application_name(name: &str) -> bool {
    super::APPLICATION_CATALOG
        .iter()
        .any(|application| application.name.eq_ignore_ascii_case(name))
}

/// Whether `classification` names a configuration Amazon EMR applies: a configuration FILE
/// of an application ([`CONFIGURATION_CLASSIFICATIONS`]), or an application's own name,
/// under which a cluster-wide property of it is set ([`is_bundled_application_name`]).
///
/// The comparison against the file names is exact: a classification is the NAME of a
/// configuration file, so a misspelling of one ("spark-default" for `spark-defaults`) names
/// no file and is not the file it resembles.
pub fn is_published_classification(classification: &str) -> bool {
    CONFIGURATION_CLASSIFICATIONS.contains(&classification)
        || is_bundled_application_name(classification)
}
