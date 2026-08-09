use sentinel_daemon::orchestrator::runtime_lifecycle::RuntimeAdapterOwner;
use sentinel_daemon::orchestrator::DaemonNanoRuntimeRegistry;

fn main() {
    let _ = std::mem::size_of::<RuntimeAdapterOwner>();
    let _ = std::mem::size_of::<DaemonNanoRuntimeRegistry>();
}
