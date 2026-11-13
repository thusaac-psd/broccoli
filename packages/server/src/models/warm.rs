//! DTOs for the contest cache pre-warm endpoints.

use serde::Serialize;
use utoipa::ToSchema;

/// Response to `POST /contests/{id}/prewarm`.
#[derive(Debug, Serialize, ToSchema)]
pub struct PrewarmResponse {
    /// The warm job id; pass to the status endpoint to follow progress.
    #[schema(example = "9f1c2e7a-2b4d-4a8e-9c3f-1a2b3c4d5e6f")]
    pub job_id: String,
    pub contest_id: i32,
    /// Distinct testcase blobs (input + answer) workers will cache.
    #[schema(example = 60)]
    pub total_blobs: u32,
    /// Live workers the job was broadcast to at publish time.
    #[schema(example = 3)]
    pub live_workers: u32,
}

/// One worker's progress within a warm job.
#[derive(Debug, Serialize, ToSchema)]
pub struct WorkerWarmStatus {
    pub worker_id: String,
    /// Blobs cached so far on this worker.
    pub warmed: u32,
    pub total: u32,
    /// Completion fraction in [0, 1].
    pub fraction: f64,
    /// `pending` | `warming` | `complete` | `error`.
    #[schema(example = "warming")]
    pub state: String,
    /// True if the worker reported progress and is no longer live (it may have
    /// died mid-warm).
    pub stale: bool,
    /// False if this live worker hasn't reported any progress yet.
    pub reported: bool,
    /// Last error message if the worker hit a blob it couldn't fetch.
    pub error: Option<String>,
}

/// Response to `GET /contests/{id}/prewarm/status`.
#[derive(Debug, Serialize, ToSchema)]
pub struct WarmStatusResponse {
    /// The job being reported, or null if no warm has been triggered.
    pub job_id: Option<String>,
    pub contest_id: i32,
    pub total_blobs: u32,
    /// Mean completion fraction across live workers, in [0, 1].
    pub overall_fraction: f64,
    /// Live workers expected to warm.
    pub workers_total: u32,
    /// Live workers that finished warming successfully.
    pub workers_complete: u32,
    pub workers: Vec<WorkerWarmStatus>,
}
