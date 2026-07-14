use std::{
    path::PathBuf,
    time::{Duration, SystemTime},
};

use ryvus_protocol::{ExecutionAttempt, InvocationEvent, InvocationRequest, InvocationResult};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Pending,
    Running,
    CancellationRequested,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub invocation_result: InvocationResult,
    pub events: Vec<InvocationEvent>,
    pub stdout: String,
    pub stderr: String,
    pub duration: Duration,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub attempt: ExecutionAttempt,
    pub request: InvocationRequest,
    pub target: ExecutionTarget,
    pub result: ExecutionResult,
    pub started_at: SystemTime,
    pub finished_at: SystemTime,
}

impl ExecutionRecord {
    pub fn new(
        request: InvocationRequest,
        target: ExecutionTarget,
        result: ExecutionResult,
        started_at: SystemTime,
        finished_at: SystemTime,
    ) -> Self {
        Self {
            attempt: request.attempt(),
            request,
            target,
            result,
            started_at,
            finished_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutionTarget {
    Process {
        command: String,
        args: Vec<String>,
        working_dir: Option<PathBuf>,
        env: std::collections::HashMap<String, String>,
    },
    Container {
        image: String,
        command: Vec<String>,
    },
    Http {
        method: String,
        url: String,
    },
}
