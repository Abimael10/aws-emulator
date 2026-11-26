// SPDX-License-Identifier: CC0-1.0

//! The parts of a bill the catalog's panels narrow to.
//!
//! A managed dashboard is published for one family of spend, and each of its panels states
//! that scope the same way: an `Expression` over one `Dimension`'s values
//! (`panels::dimension_filter`). The service lists below are the values the recorded
//! `SERVICE` filters of the compute, database and EC2 panels carry, so the family a panel
//! covers is written here once and shared by every panel of that dashboard rather than
//! repeated per panel.

/// The `SERVICE` dimension values the compute panels narrow to, as the recorded compute
/// panels' own filter lists them.
pub(super) const COMPUTE_SERVICES: [&str; 12] = [
    "Amazon EC2 Container Registry (ECR)",
    "Amazon Elastic Container Service for Kubernetes",
    "EC2 - Other",
    "Amazon Elastic Compute Cloud - Compute",
    "Amazon Elastic Container Service",
    "AWS Lambda",
    "AWS App Runner",
    "Amazon Lightsail",
    "Amazon Elastic Container Registry Public",
    "AWS Parallel Computing Service",
    "Amazon EKS Anywhere",
    "AWS Outposts",
];

/// The `SERVICE` dimension values the database panels narrow to, as the recorded database
/// panels' own filter lists them.
pub(super) const DATABASE_SERVICES: [&str; 13] = [
    "Amazon Relational Database Service",
    "Amazon DynamoDB",
    "Amazon ElastiCache",
    "Amazon OpenSearch Service",
    "Amazon Redshift",
    "Amazon MemoryDB",
    "Amazon DocumentDB (with MongoDB compatibility)",
    "Amazon Neptune",
    "Amazon Keyspaces (for Apache Cassandra)",
    "Amazon Timestream",
    "AWS Database Migration Service",
    "Aurora DSQL",
    "Oracle Database@AWS",
];

/// The `SERVICE` dimension values the EC2 panels narrow to, as the recorded EC2 panels' own
/// filter lists them: the instances, what they are billed beside, and the registry their
/// images come from.
pub(super) const EC2_SERVICES: [&str; 3] = [
    "Amazon EC2 Container Registry (ECR)",
    "EC2 - Other",
    "Amazon Elastic Compute Cloud - Compute",
];

/// The instance usage a reservation covers, as the recorded reservation-coverage panel's own
/// filter states it: the compute charge of Amazon EC2 alone.
pub(super) const EC2_COMPUTE_SERVICE: [&str; 1] = ["Amazon Elastic Compute Cloud - Compute"];

/// The container services inside [`COMPUTE_SERVICES`], for the panel published over the
/// container part of the compute bill.
pub(super) const CONTAINER_SERVICES: [&str; 5] = [
    "Amazon Elastic Container Service",
    "Amazon Elastic Container Service for Kubernetes",
    "Amazon EC2 Container Registry (ECR)",
    "Amazon Elastic Container Registry Public",
    "Amazon EKS Anywhere",
];

/// The serverless function service inside [`COMPUTE_SERVICES`], for the panel published over
/// that part of the compute bill.
pub(super) const LAMBDA_SERVICE: [&str; 1] = ["AWS Lambda"];

/// The `PURCHASE_TYPE` dimension values the commitment panels narrow to, as a bill spells
/// them — on-demand usage as the recorded on-demand panel's own filter carries it.
pub(super) const PURCHASE_TYPE_ON_DEMAND: [&str; 1] = ["On Demand Instances"];
pub(super) const PURCHASE_TYPE_RESERVED_INSTANCES: [&str; 1] = ["Reserved Instances"];
pub(super) const PURCHASE_TYPE_SAVINGS_PLANS: [&str; 1] = ["Savings Plans"];
