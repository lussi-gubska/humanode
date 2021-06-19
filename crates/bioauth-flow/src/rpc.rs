//! RPC interface for the bioauth flow.

use std::sync::Arc;

use futures::channel::oneshot;
use futures::compat::Compat;
use futures::lock::BiLock;
use jsonrpc_core::futures::Future;
use jsonrpc_core::Error as RpcError;
use jsonrpc_core::ErrorCode;
use jsonrpc_derive::rpc;
use primitives_bioauth::LivenessData;

use crate::flow::LivenessDataProvider;

/// A result type that wraps.
pub type Result<T> = std::result::Result<T, RpcError>;

/// A futures that resolves to the specified `T`, or an [`RpcError`].
pub type FutureResult<T> = Box<dyn Future<Item = T, Error = RpcError> + Send>;

/// The API exposed via JSON-RPC.
#[rpc]
pub trait BioauthApi {
    /// Get a FaceTec Session Token.
    #[rpc(name = "bioauth_getFacetecSessionToken")]
    fn get_facetec_session_token(&self) -> FutureResult<String>;

    /// Provide the liveness data for the currently running enrollemnt or authentication process.
    #[rpc(name = "bioauth_provideLivenessData")]
    fn provide_liveness_data(&self, liveness_data: LivenessData) -> FutureResult<()>;
}

/// The RPC implementation.
pub struct Bioauth<C>
where
    C: AsRef<robonode_client::Client>,
{
    /// The underlying implementation.
    /// We have to wrap it with `Arc` because we have to provide compatibility with `futures` `0.1`,
    /// up until `substrate` switches to 0.16+ version of the `jsonrpc-core`.
    inner: Arc<Inner<C>>,
}

impl<C> Bioauth<C>
where
    C: AsRef<robonode_client::Client>,
{
    /// Create a new [`Bioauth`] API implementation.
    pub fn new(
        robonode_client: C,
        liveness_data_tx_slot: BiLock<Option<oneshot::Sender<LivenessData>>>,
    ) -> Self {
        let inner = Inner {
            client: robonode_client,
            liveness_data_tx_slot,
        };
        Self {
            inner: Arc::new(inner),
        }
    }
}

impl<C> BioauthApi for Bioauth<C>
where
    C: AsRef<robonode_client::Client> + Send + Sync + 'static,
{
    /// Wrap `get_facetec_session_token` with `futures` `0.1` compat layer.
    fn get_facetec_session_token(&self) -> FutureResult<String> {
        let inner = Arc::clone(&self.inner);
        let call = inner.get_facetec_session_token();
        Box::new(Compat::new(Box::pin(call)))
    }

    /// Wrap `provide_liveness_data` with `futures` `0.1` compat layer.
    fn provide_liveness_data(&self, liveness_data: LivenessData) -> FutureResult<()> {
        let inner = Arc::clone(&self.inner);
        let call = inner.provide_liveness_data(liveness_data);
        Box::new(Compat::new(Box::pin(call)))
    }
}

/// The underlying implementation of the RPC part, extracted into a subobject to ensure the compat
/// is properly set up.
struct Inner<C>
where
    C: AsRef<robonode_client::Client>,
{
    /// The robonode client, used for fetching the FaceTec Session Token.
    client: C,
    /// The liveness data provider sink.
    liveness_data_tx_slot: BiLock<Option<oneshot::Sender<LivenessData>>>,
}

impl<C> Inner<C>
where
    C: AsRef<robonode_client::Client>,
{
    /// Get the FaceTec Session Token.
    async fn get_facetec_session_token(self: Arc<Self>) -> Result<String> {
        let res = self
            .client
            .as_ref()
            .get_facetec_session_token()
            .await
            .map_err(|err| RpcError {
                code: ErrorCode::ServerError(1),
                message: format!("request to the robonode failed: {}", err),
                data: None,
            })?;
        Ok(res.session_token)
    }

    /// Collect the liveness data and provide to the consumer.
    async fn provide_liveness_data(self: Arc<Self>, liveness_data: LivenessData) -> Result<()> {
        let maybe_tx = {
            let mut maybe_tx_guard = self.liveness_data_tx_slot.lock().await;
            maybe_tx_guard.take() // take the guarded option value and release the lock asap
        };
        let tx = maybe_tx.ok_or_else(|| RpcError {
            code: ErrorCode::InternalError,
            message: "Flow is not engaged, unable to accept liveness data".into(),
            data: None,
        })?;
        tx.send(liveness_data).map_err(|_| RpcError {
            code: ErrorCode::InternalError,
            message: "Flow was aborted before the liveness data could be submitted".into(),
            data: None,
        })?;
        Ok(())
    }
}

/// Provider implements a [`LivenessDataProvider`].
pub struct Provider {
    /// The shared liveness data sender slot, that we can swap with our ephemernal
    /// channel upon a liveness data reuqest.
    liveness_data_tx_slot: BiLock<Option<oneshot::Sender<LivenessData>>>,
}

impl Provider {
    /// Construct a new [`Provider`].
    pub fn new(liveness_data_tx_slot: BiLock<Option<oneshot::Sender<LivenessData>>>) -> Self {
        Self {
            liveness_data_tx_slot,
        }
    }
}

#[async_trait::async_trait]
impl LivenessDataProvider for Provider {
    type Error = oneshot::Canceled;

    async fn provide(&mut self) -> std::result::Result<LivenessData, Self::Error> {
        let (tx, rx) = oneshot::channel();

        {
            let mut maybe_tx_guard = self.liveness_data_tx_slot.lock().await;
            maybe_tx_guard.insert(tx); // insert a new sender value and free the lock asap
        }

        Ok(rx.await?)
    }
}