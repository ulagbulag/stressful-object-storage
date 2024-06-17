mod args;
mod session;

use anyhow::anyhow;
use ark_core::signal::FunctionSignal;
use tokio::runtime::Runtime;
use tracing::{error, info};

fn main() {
    #[cfg(feature = "sas")]
    ::sas::init();

    let rt = Runtime::new().expect("failed to create a tokio runtime");
    rt.block_on(main_async())
}

async fn main_async() {
    ::ark_core::tracer::init_once();
    info!("Welcome to kubegraph function service!");

    let signal = FunctionSignal::default().trap_on_panic();
    if let Err(error) = signal.trap_on_sigint() {
        error!("{error}");
        return;
    }

    info!("Booting...");
    let session = match self::session::ObjectStorageSession::try_default().await {
        Ok(session) => session,
        Err(error) => {
            signal
                .panic(anyhow!("failed to init function service: {error}"))
                .await
        }
    };

    info!("Creating session tasks...");
    let handler_session = session.spawn(signal.clone());

    info!("Ready");
    signal.wait_to_terminate().await;

    info!("Terminating...");
    if let Err(error) = handler_session.await {
        error!("{error}");
    };

    signal.exit().await
}
