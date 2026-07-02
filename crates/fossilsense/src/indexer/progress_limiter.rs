use std::time::Instant;

use crate::progress::{IndexStats, IndexStatus};

const PROGRESS_FILE_STEP: usize = 128;
const PROGRESS_MIN_INTERVAL_MS: u128 = 250;

pub(super) struct ProgressLimiter {
    last_at: Instant,
    last_processed: usize,
}

impl ProgressLimiter {
    pub(super) fn new() -> Self {
        Self {
            last_at: Instant::now(),
            last_processed: 0,
        }
    }

    pub(super) fn maybe_emit(
        &mut self,
        progress: &mut impl FnMut(IndexStatus),
        workspace_display: &str,
        stats: &IndexStats,
        phase: &str,
    ) {
        let processed_delta = stats.processed_files.saturating_sub(self.last_processed);
        let interval_ms = self.last_at.elapsed().as_millis();
        if stats.processed_files == stats.total_files
            || processed_delta >= PROGRESS_FILE_STEP
            || interval_ms >= PROGRESS_MIN_INTERVAL_MS
        {
            self.emit_now(progress, workspace_display, stats, phase);
        }
    }

    pub(super) fn emit_now(
        &mut self,
        progress: &mut impl FnMut(IndexStatus),
        workspace_display: &str,
        stats: &IndexStats,
        phase: &str,
    ) {
        progress(IndexStatus::indexing_phase(
            workspace_display.to_string(),
            stats,
            phase,
        ));
        self.last_at = Instant::now();
        self.last_processed = stats.processed_files;
    }

    pub(super) fn emit_if_changed(
        &mut self,
        progress: &mut impl FnMut(IndexStatus),
        workspace_display: &str,
        stats: &IndexStats,
        phase: &str,
    ) {
        if stats.processed_files != self.last_processed {
            self.emit_now(progress, workspace_display, stats, phase);
        }
    }
}
