use iox_clap_blocks::run_config::ServingReadinessState;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tonic::{Request, Status};

#[derive(Debug, Clone)]
pub struct ServingReadiness(Arc<AtomicBool>);

impl ServingReadiness {
    pub fn new(value: Arc<AtomicBool>) -> Self {
        Self(value)
    }

    pub fn get(&self) -> ServingReadinessState {
        self.0.load(Ordering::SeqCst).into()
    }

    pub fn set(&self, state: ServingReadinessState) {
        self.0.store(state.into(), Ordering::SeqCst)
    }

    /// Implements the gRPC interceptor that returns SERVICE_UNAVAILABLE gRPC status
    /// if the service is not ready.
    pub fn into_interceptor(
        self,
    ) -> impl FnMut(Request<()>) -> Result<Request<()>, Status> + Clone {
        move |req| match self.get() {
            ServingReadinessState::Unavailable => {
                Err(Status::unavailable("service not ready to serve"))
            }
            ServingReadinessState::Serving => Ok(req),
        }
    }
}

impl From<Arc<AtomicBool>> for ServingReadiness {
    fn from(value: Arc<AtomicBool>) -> Self {
        Self::new(value)
    }
}

impl From<AtomicBool> for ServingReadiness {
    fn from(value: AtomicBool) -> Self {
        Arc::new(value).into()
    }
}

impl From<ServingReadinessState> for ServingReadiness {
    fn from(value: ServingReadinessState) -> Self {
        AtomicBool::new(value.into()).into()
    }
}
