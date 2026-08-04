use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify};
use tokio::time::{interval, Duration};
use tower_lsp::lsp_types::notification::Notification;
use tower_lsp::Client;

use super::workspace::{EngineSnapshot, WorkspaceSession};
use crate::memory_report::{MemoryReport, OpenDocumentsMemoryReport};
use crate::resource::{current_process_memory_bytes, index_directory_disk_bytes};

const REPORT_INTERVAL: Duration = Duration::from_secs(2);
/// The index directory walk stays off the 2-second memory cadence; disk usage
/// moves slowly, so it refreshes every fifth tick (~10 seconds).
const DISK_REFRESH_TICKS: u32 = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResourceUsage {
    memory_bytes: u64,
    index_disk_bytes: u64,
    memory: MemoryReport,
    timestamp: u64,
}

enum ResourceUsageNotification {}

impl Notification for ResourceUsageNotification {
    type Params = ResourceUsage;
    const METHOD: &'static str = "fossilsense/resourceUsage";
}

/// Push `fossilsense/resourceUsage` every two seconds until shutdown. The
/// protocol adapter stays under `server/`; collectors remain reusable and
/// protocol-neutral in `resource` and `memory_report`.
pub(super) fn spawn_resource_usage_reporter(
    client: Client,
    workspace_roots: Arc<Mutex<Vec<PathBuf>>>,
    session: WorkspaceSession,
    shutdown: Arc<Notify>,
) {
    tokio::spawn(async move {
        let mut ticker = interval(REPORT_INTERVAL);
        // `interval` ticks immediately. Delay the first notification so a new
        // workspace has time to create its index directory.
        ticker.tick().await;
        let mut tick_count: u32 = 0;
        let mut index_disk_bytes = 0u64;
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    tick_count = tick_count.wrapping_add(1);
                    if tick_count == 1 || tick_count.is_multiple_of(DISK_REFRESH_TICKS) {
                        let roots = workspace_roots.lock().await.clone();
                        index_disk_bytes = tokio::task::spawn_blocking(move || {
                            index_directory_disk_bytes(&roots)
                        })
                        .await
                        .unwrap_or(0);
                    }
                    let memory_bytes = current_process_memory_bytes();
                    let snapshots: Vec<Arc<EngineSnapshot>> = session
                        .cache
                        .engine_snapshots
                        .lock()
                        .await
                        .values()
                        .cloned()
                        .collect();
                    let (document_count, document_bytes) = session.documents.memory_stats().await;
                    let overlay_bytes = session.cache.overlay_memory_bytes().await;
                    let timestamp = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let ledger = session.cache.clone();
                    // Static per-generation numbers are memoized per (root,
                    // epoch); only the first tick after a publish walks the
                    // snapshot structures, and it does so off the runtime.
                    let memory = tokio::task::spawn_blocking(move || {
                        let (statics, caches) = ledger.memory_observation(&snapshots);
                        MemoryReport::assemble(
                            &statics,
                            &caches,
                            OpenDocumentsMemoryReport {
                                bytes: document_bytes.saturating_add(overlay_bytes) as u64,
                                document_count: document_count as u64,
                                overlay_bytes: overlay_bytes as u64,
                            },
                            memory_bytes,
                            index_disk_bytes,
                            timestamp,
                        )
                    })
                    .await
                    .unwrap_or_default();
                    client
                        .send_notification::<ResourceUsageNotification>(ResourceUsage {
                            memory_bytes,
                            index_disk_bytes,
                            memory,
                            timestamp,
                        })
                        .await;
                }
                _ = shutdown.notified() => break,
            }
        }
    });
}
