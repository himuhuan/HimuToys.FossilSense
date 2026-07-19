use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify};
use tokio::time::{interval, Duration};
use tower_lsp::lsp_types::notification::Notification;
use tower_lsp::Client;

use crate::resource::{current_process_memory_bytes, index_directory_disk_bytes};

const REPORT_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResourceUsage {
    memory_bytes: u64,
    index_disk_bytes: u64,
    timestamp: u64,
}

enum ResourceUsageNotification {}

impl Notification for ResourceUsageNotification {
    type Params = ResourceUsage;
    const METHOD: &'static str = "fossilsense/resourceUsage";
}

/// Push `fossilsense/resourceUsage` every five seconds until shutdown. The
/// protocol adapter stays under `server/`; collectors remain reusable and
/// protocol-neutral in `resource`.
pub(super) fn spawn_resource_usage_reporter(
    client: Client,
    workspace_roots: Arc<Mutex<Vec<PathBuf>>>,
    shutdown: Arc<Notify>,
) {
    tokio::spawn(async move {
        let mut ticker = interval(REPORT_INTERVAL);
        // `interval` ticks immediately. Delay the first notification so a new
        // workspace has time to create its index directory.
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let roots = workspace_roots.lock().await.clone();
                    let memory_bytes = current_process_memory_bytes();
                    let index_disk_bytes = tokio::task::spawn_blocking(move || {
                        index_directory_disk_bytes(&roots)
                    })
                    .await
                    .unwrap_or(0);
                    let timestamp = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    client
                        .send_notification::<ResourceUsageNotification>(ResourceUsage {
                            memory_bytes,
                            index_disk_bytes,
                            timestamp,
                        })
                        .await;
                }
                _ = shutdown.notified() => break,
            }
        }
    });
}
