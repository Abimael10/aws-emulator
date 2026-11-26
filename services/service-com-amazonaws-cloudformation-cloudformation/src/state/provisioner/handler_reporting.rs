// SPDX-License-Identifier: CC0-1.0

//! How a type's resource PROVIDER reports a refusal: the wording it puts a refused call's
//! message in, and the `HandlerErrorCode` class it files the failure under.
//!
//! One file of [`super`]'s per-type half of provisioning, beside the create and delete arms
//! the same `Provisioner` dispatches: what a provider SAYS about a resource it could not
//! bring up is per type exactly as what it CALLS is, and keeping the two halves in separate
//! files leaves each with one concern.

use super::*;

impl Provisioner {
    /// How THIS type's resource provider words a refusal of the owning service, which is the
    /// message CloudFormation quotes in the failed resource's status reason
    /// ([`super::provisioning`]).
    ///
    /// A provider reports the refusal of the service it called, and most of them report that
    /// service's message as it came — the diagnostic a client of that service would read,
    /// carrying that service's own name and status. A provider written against the
    /// CloudFormation resource-provider plugin reports it through the exception class of the
    /// `HandlerErrorCode` it classified the refusal under, and THAT class's wording is part
    /// of the message rather than only of the envelope: an `InvalidRequest` refusal reaches
    /// the reason as `Invalid request provided: <the service's message>`, which is what an
    /// `AWS::OpenSearchService::Domain` whose cluster its service will not provision is
    /// reported with.
    ///
    /// Which of the two a type is, is that type's provider's and cannot be derived from the
    /// type's name, so it is stated per type here — the same per-type half of provisioning
    /// the create and delete arms are. A type not named below reports the message unchanged.
    ///
    /// `token` is the token of the resource request this refusal answered, which the provider
    /// of an `AWS::MSK::*` type states at the head of its message
    /// ([`MSK_HANDLER_REQUEST_TOKEN_LABEL`]) — the same token the event's own
    /// `(RequestToken: …)` reports, since one resource request earned both.
    pub(in crate::state) fn handler_message(self, class: &str, message: &str, token: &str) -> String {
        let plugin_worded = matches!(
            self,
            Self::OpenSearchServiceDomain
            // The WARM POOL's provider is written against that plugin too, so an
            // invalid-request refusal of Amazon EC2 Auto Scaling's reaches the reason as
            // `Invalid request provided: <the service's message>` — as the recorded
            // `ResourceStatusReason` of an `AWS::AutoScaling::WarmPool` whose declared
            // `PoolState` is outside the set that member admits carries it (in the behavioral
            // corpus `simparity-behavioral-aws-v1`, check
            // `autoscaling__cloudformation.deploy-asg-warmpool-poolstate-invalid-enum-rejected`,
            // whose reason reads `Resource handler returned message: "Invalid request provided:
            // <the service's sentence> (Service: AutoScaling, Status Code: 400, Request ID: …)
            // (SDK Attempt Count: …)" (RequestToken: …, HandlerErrorCode: InvalidRequest)`).
            // Every refusal of this type that falls in that class is worded this way, whichever
            // call made it, and a refusal of any other class is reported as it came. The
            // group's own provider is NOT one of these — its recorded reason carries the
            // service's message alone ([`super::provisioning::sdk_diagnostics`]) — and neither
            // is any other type of this service here, no recorded reason of one showing the
            // plugin's wording: which of the two a type is, is its provider's.
            | Self::AutoScalingWarmPool
        );
        if plugin_worded && class == super::provisioning::HANDLER_ERROR_INVALID_REQUEST {
            return format!("{INVALID_REQUEST_PROVIDED}{message}");
        }
        // Every Amazon MSK type's provider opens its message with the resource request's own
        // token, whatever class the refusal falls in: it is how that provider reports a
        // refusal rather than something it says about one kind of refusal.
        if matches!(
            self,
            Self::MskCluster | Self::MskServerlessCluster | Self::MskConfiguration
        ) {
            return format!("[{MSK_HANDLER_REQUEST_TOKEN_LABEL}: {token}] {message}");
        }
        // The `AWS::ElasticBeanstalk::ConfigurationTemplate` provider reports the call it made
        // as an OPERATION that failed, quoting the exception its SDK raised inside its own
        // sentence ([`elastic_beanstalk_operation_failure`]) — whatever class the refusal falls
        // in, since that is how this provider words a refusal rather than something it says
        // about one kind of refusal. The ENVIRONMENT's provider is not one of these: its
        // recorded reason carries the service's message alone, which is what makes this
        // per-type rather than per-service.
        if matches!(self, Self::ElasticBeanstalkConfigurationTemplate) {
            return elastic_beanstalk_operation_failure(message);
        }
        message.to_string()
    }

    /// Which `HandlerErrorCode` class this type's provider reports a resource it could NOT
    /// BRING UP under — a resource whose owning service accepted the call and then settled it
    /// somewhere the provider does not serve the resource from
    /// ([`super::provisioning::handler_reported_failure_reason`]).
    ///
    /// Nothing about the request was wrong in either class, so what separates them is what the
    /// PROVIDER reported: a provider that gives up on a resource with its own account of a
    /// resource that DID NOT STABILIZE classifies it as exactly that
    /// ([`super::provisioning::HANDLER_ERROR_NOT_STABILIZED`]), while one that passes on the
    /// owning service's own failure reports it under the class for a service that failed
    /// ([`super::provisioning::HANDLER_ERROR_SERVICE_FAILED`]). Which of the two a type's
    /// provider does is that provider's own and cannot be derived from the sentence it worded,
    /// so it is stated per type here beside the rest of the per-type half of provisioning.
    ///
    /// `AWS::Pipes::Pipe` is the first kind: its provider holds the pipe open until EventBridge
    /// Pipes reports it resting and reports a pipe that service settled in one of its FAILED
    /// states as one that did not stabilize ([`event_pipes::readiness`]), which is the class the
    /// recorded reason of such a resource carries. A type not named below reports the class for
    /// a service that failed.
    pub(in crate::state) fn reported_failure_class(self) -> &'static str {
        if matches!(self, Self::PipesPipe) {
            return super::provisioning::HANDLER_ERROR_NOT_STABILIZED;
        }
        super::provisioning::HANDLER_ERROR_SERVICE_FAILED
    }

    /// Which `HandlerErrorCode` class this type's provider puts a refusal carrying `code` in,
    /// where that is NOT the class the code itself falls in
    /// ([`super::provisioning::handler_error_code`] computes `classified`).
    ///
    /// `NotFound` is the class of a refusal about the resource the handler MANAGES — the one
    /// it was asked to read, update or delete. A refusal naming a DIFFERENT resource, one the
    /// declaration merely REFERENCES, says the declaration named something that is not there:
    /// the request the handler was given is the thing at fault, so its provider reports it as
    /// `InvalidRequest` and a client reading the class sees a template to fix rather than a
    /// resource that vanished.
    ///
    /// Which of a type's refusals are about a referenced resource is that type's own, so it is
    /// stated per type here beside the rest of the per-type half of provisioning. A type not
    /// named below reports the class its code falls in.
    ///
    /// A type whose provider does not map one of the owning service's own error codes is
    /// named here too: the plugin reports an exception it has no mapping for under its class
    /// for a service that failed ([`super::provisioning::HANDLER_ERROR_SERVICE_FAILED`]),
    /// whatever the code itself reads like — which is the class the recorded reason of a
    /// failed `AWS::CodePipeline::Pipeline` carries for CodePipeline's
    /// `InvalidStructureException` (in the behavioral corpus `simparity-behavioral-aws-v1`,
    /// check
    /// `cloudformation__codepipeline.deploy-codepipeline-pipeline-unassumable-role-rolled-back`,
    /// whose reason closes `HandlerErrorCode: GeneralServiceException`). It is that CODE that
    /// decides the class, as everywhere else here, and never the message the refusal carried:
    /// every refusal that service answers under it is reported the same way, while its other
    /// declaration exceptions keep the class their own codes fall in.
    pub(in crate::state) fn handler_error_class(self, code: &str, classified: &'static str) -> &'static str {
        if matches!(self, Self::CodePipelinePipeline) && code == CODEPIPELINE_INVALID_STRUCTURE {
            return super::provisioning::HANDLER_ERROR_SERVICE_FAILED;
        }
        // The `AWS::ACMPCA::Permission` provider maps none of Amazon Web Services Private CA's
        // validation refusals either — a grant naming a principal that service does not grant
        // to is one — so the plugin reports it under the same class for a service that failed.
        // Recorded behaviour, read from the reason of the failed resource of a stack declaring
        // such a grant (in the behavioral corpus `simparity-behavioral-aws-v1`, check
        // `acm-pca__cloudformation.deploy-cfn-acmpca-permission-invalid-principal-rejected`,
        // whose reason closes `HandlerErrorCode: GeneralServiceException`), and decided — as
        // everywhere here — by the CODE that service answered rather than by its message.
        if matches!(self, Self::AcmPcaPermission) && code == ACMPCA_VALIDATION_EXCEPTION {
            return super::provisioning::HANDLER_ERROR_SERVICE_FAILED;
        }
        // The `AWS::ApiGatewayV2::Api` provider reads the definition a declaration holds in
        // Amazon S3 itself ([`super::provisioning::apigateway_definitions`]), and a read it
        // could not make is an exception the plugin has no mapping for: the provider
        // classified that failure itself, and the class it carries as its code is what it is
        // reported under. Recorded behaviour, read from the reason of the failed resource of a
        // stack whose api declares an object that is not there (in the behavioral corpus
        // `simparity-behavioral-aws-v1`, check `apigatewayv2__cloudformation.deploy-\
        // apigatewayv2-cfn-bodys3location-missing-object-fails-stack`, whose reason closes
        // `HandlerErrorCode: GeneralServiceException`). Its refusals of a modelled error of
        // that service keep the class their own codes fall in.
        if matches!(self, Self::ApiGatewayV2Api)
            && code == super::provisioning::apigateway_definitions::READ_FAILED
        {
            return super::provisioning::HANDLER_ERROR_SERVICE_FAILED;
        }
        // Each `AWS::AmplifyUIBuilder::*` provider maps none of Amplify UI Builder's
        // validation refusals either — an entity name that service does not accept is one —
        // and the plugin those three providers are written against falls back to its own
        // INTERNAL-FAILURE class for an exception it has no mapping for, rather than to the
        // class for a service that failed. Recorded behaviour, read from the reason of the
        // failed resource of a stack declaring such a form (in the behavioral corpus
        // `simparity-behavioral-aws-v1`, check `amplify__amplifyuibuilder__\
        // cloudformation.deploy-amplifyuibuilder-form-invalid-name-rejected-and-rolled-back`,
        // whose reason closes `HandlerErrorCode: InternalFailure`), and decided — as
        // everywhere here — by what that service ANSWERED rather than by its message, so
        // every refusal of a request it answers is reported this way whichever call made it.
        if matches!(
            self,
            Self::AmplifyUiBuilderComponent
                | Self::AmplifyUiBuilderForm
                | Self::AmplifyUiBuilderTheme
        ) && code == AMPLIFYUIBUILDER_REFUSED_REQUEST
        {
            return super::provisioning::HANDLER_ERROR_INTERNAL_FAILURE;
        }
        // The `AWS::ElasticBeanstalk::ConfigurationTemplate` provider maps none of Elastic
        // Beanstalk's parameter-value refusals — a solution stack name that service does not
        // publish is one — so the plugin reports it under the same class for a service that
        // failed. Recorded behaviour, read from the reason of the failed resource of a stack
        // declaring such a configuration (in the behavioral corpus
        // `simparity-behavioral-aws-v1`, check `cloudformation__elasticbeanstalk.deploy-\
        // elasticbeanstalk-configurationtemplate-invalid-solutionstack-rejected-and-rolled-\
        // back`, whose reason closes `HandlerErrorCode: GeneralServiceException`), and decided
        // — as everywhere here — by the CODE that service answered rather than by its message.
        // The ENVIRONMENT's provider maps that same code to the invalid-request class, which
        // is what makes this per-type rather than per-service.
        if matches!(self, Self::ElasticBeanstalkConfigurationTemplate)
            && code == ELASTIC_BEANSTALK_INVALID_PARAMETER_VALUE
        {
            return super::provisioning::HANDLER_ERROR_SERVICE_FAILED;
        }
        // The `AWS::CertificateManager::Certificate` provider maps none of Amazon Route 53's
        // refusals of the change batch it publishes the certificate's DNS validation record
        // with: that batch is another service's call, and the plugin reports an exception it
        // has no mapping for under its class for a service that failed. Recorded behaviour,
        // read from the reason of the failed certificate of a stack whose
        // `DomainValidationOptions` names a zone that is not authoritative for the domain (in
        // the behavioral corpus `simparity-behavioral-aws-v1`, check `acm__cloudformation__\
        // route53.cfn-certificate-hostedzoneid-not-covering-domain-rejected`, whose reason
        // closes `HandlerErrorCode: GeneralServiceException`), and decided — as everywhere
        // here — by the CODE that service answered rather than by its message, so every
        // refusal of that batch is reported this way whichever zone it named. ACM's own
        // refusals keep the class their own codes fall in.
        if matches!(self, Self::AcmCertificate) && code.contains(INVALID_CHANGE_BATCH) {
            return super::provisioning::HANDLER_ERROR_SERVICE_FAILED;
        }
        // The `AWS::AppFlow::ConnectorProfile` provider maps none of Amazon AppFlow's
        // CONNECTOR-ENDPOINT exceptions: a connection test that service could not complete is
        // the connector failing rather than the declaration being wrong, and the plugin reports
        // an exception it has no mapping for under its class for a service that failed. Recorded
        // behaviour, read from the reason of the failed resource of a stack whose profile
        // declares a Data API role that service's connector cannot take on (in the behavioral
        // corpus `simparity-behavioral-aws-v1`, check `appflow__cloudformation.deploy-appflow-\
        // connector-profile-redshift-bad-data-api-role-update-rejected-and-rolled-back`, whose
        // reason closes `HandlerErrorCode: GeneralServiceException`), and decided — as everywhere
        // here — by the CODE that service answered rather than by its message. Its refusals of
        // the request itself keep the class their own codes fall in.
        if matches!(self, Self::AppFlowConnectorProfile)
            && code == APPFLOW_CONNECTOR_SERVER_EXCEPTION
        {
            return super::provisioning::HANDLER_ERROR_SERVICE_FAILED;
        }
        let references_another_resource = match self {
            // A record set is written INSIDE a hosted zone its declaration names
            // (`HostedZoneId`/`HostedZoneName`), and the record set itself is the resource
            // this handler manages: a zone Route 53 does not hold (`NoSuchHostedZone`) is a
            // declaration naming a zone that is not there. A GROUP of record sets names its
            // zone the same way and is in the same position.
            Self::Route53RecordSet | Self::Route53RecordSetGroup => {
                code.contains(NO_SUCH_HOSTED_ZONE)
            }
            // A certificate's validation record is written inside the zone its
            // `DomainValidationOptions` names, while the resource this handler manages is the
            // CERTIFICATE: a zone Route 53 does not hold is the same declaration naming a
            // zone that is not there, so it reads the same way here.
            Self::AcmCertificate => code.contains(NO_SUCH_HOSTED_ZONE),
            _ => false,
        };
        if references_another_resource {
            return super::provisioning::HANDLER_ERROR_INVALID_REQUEST;
        }
        classified
    }
}
