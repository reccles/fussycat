//! The one adapter every provider goes through: the neutral request to
//! rig's, a retried call, rig's reply back to neutral. What differs per
//! provider is its [`Dialect`]: a hook on the way out and one on the way
//! back.

use std::future::Future;

use fussycat_core::provider::{Provider, ProviderError, Request, Response};
use rig_core::DynModel;
use rig_core::completion::{CompletionRequest, CompletionResponse};
use rig_core::operation::Completion;

use super::messages::{from_rig, to_rig};
use super::retry::{self, Policy};

/// What one provider adds to the shared translation. Both hooks default
/// to doing nothing.
pub trait Dialect: Send + Sync {
    /// Last changes to the request rig sends: options only this provider
    /// takes. `request` is the neutral one it was made from.
    fn request(&self, _request: &Request, rig: CompletionRequest) -> CompletionRequest {
        rig
    }

    /// What the reply holds that [`from_rig`] can't read: a provider's own
    /// fields.
    fn response(&self, _rig: &CompletionResponse, _neutral: &mut Response) {}
}

/// A provider's model, its retry policy and its dialect.
pub struct Adapter {
    pub(super) model: DynModel<Completion>,
    retry: Policy,
    dialect: Box<dyn Dialect>,
}

impl Adapter {
    pub(super) fn new(
        model: impl Into<DynModel<Completion>>,
        retry: Policy,
        dialect: impl Dialect + 'static,
    ) -> Self {
        Self {
            model: model.into(),
            retry,
            dialect: Box::new(dialect),
        }
    }
}

impl Provider for Adapter {
    fn complete(
        &self,
        request: &Request,
    ) -> impl Future<Output = Result<Response, ProviderError>> + Send {
        let rig_request = to_rig(request).map(|rig| self.dialect.request(request, rig));
        async move {
            let rig_request = rig_request?;
            let (response, retries) =
                retry::call(self.retry, || self.model.call(rig_request.clone())).await?;
            let mut neutral = from_rig(&response, retries)?;
            self.dialect.response(&response, &mut neutral);
            Ok(neutral)
        }
    }
}
