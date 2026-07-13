use std::{collections::HashMap, path::PathBuf, process::Stdio, sync::Arc};

use async_trait::async_trait;
use ryvus_protocol::{
    InvocationEvent, InvocationRequest, InvocationResult, TerminationReason, WorkerId,
};
use serde::Deserialize;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    time::Instant,
};

use crate::{InvocationWorker, InvocationWorkerFactory, StartedWorker, WorkerError};

const WORKER_PROTOCOL: &str = "framed";

#[derive(Debug, Clone)]
pub struct ProcessWorkerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<PathBuf>,
    pub env: HashMap<String, String>,
}

impl ProcessWorkerConfig {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            working_dir: None,
            env: HashMap::new(),
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn working_dir(mut self, working_dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(working_dir.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }
}

pub struct ProcessInvocationWorkerFactory {
    config: ProcessWorkerConfig,
}

impl ProcessInvocationWorkerFactory {
    pub fn new(config: ProcessWorkerConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl InvocationWorkerFactory for ProcessInvocationWorkerFactory {
    async fn start(&self, _request: &InvocationRequest) -> Result<StartedWorker, WorkerError> {
        let mut command = Command::new(&self.config.command);
        command
            .args(&self.config.args)
            .envs(&self.config.env)
            .env("RYVUS_WORKER_PROTOCOL", WORKER_PROTOCOL)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if let Some(working_dir) = &self.config.working_dir {
            command.current_dir(working_dir);
        }
        let mut child = command.spawn().map_err(WorkerError::Start)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| WorkerError::Protocol("worker stdin was not piped".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| WorkerError::Protocol("worker stdout was not piped".to_string()))?;
        let worker = Arc::new(ProcessInvocationWorker {
            child: Mutex::new(Some(child)),
            stdin: Mutex::new(Some(stdin)),
            stdout: Mutex::new(BufReader::new(stdout)),
        });

        Ok(StartedWorker {
            worker_id: WorkerId::new(),
            worker,
        })
    }
}

pub struct ProcessInvocationWorker {
    child: Mutex<Option<Child>>,
    stdin: Mutex<Option<ChildStdin>>,
    stdout: Mutex<BufReader<ChildStdout>>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WorkerFrame {
    Ready,
    Event { event: InvocationEvent },
    Result { result: InvocationResult },
}

#[async_trait]
impl InvocationWorker for ProcessInvocationWorker {
    async fn wait_ready(&self, deadline: Instant) -> Result<(), WorkerError> {
        match self.read_frame(deadline).await? {
            Some(WorkerFrame::Ready) => Ok(()),
            Some(_) => Err(WorkerError::Protocol(
                "worker emitted a non-ready frame during startup".to_string(),
            )),
            None => Err(WorkerError::Protocol(
                "worker stdout ended before the ready frame".to_string(),
            )),
        }
    }

    async fn invoke(
        &self,
        request: InvocationRequest,
        deadline: Instant,
    ) -> Result<InvocationResult, WorkerError> {
        let payload = serde_json::to_vec(&request).map_err(WorkerError::Serialize)?;
        let mut stdin = self.stdin.lock().await.take().ok_or_else(|| {
            WorkerError::Protocol("worker stdin was already consumed".to_string())
        })?;
        stdin
            .write_all(&payload)
            .await
            .map_err(WorkerError::Process)?;
        stdin.write_all(b"\n").await.map_err(WorkerError::Process)?;
        stdin.shutdown().await.map_err(WorkerError::Process)?;
        drop(stdin);

        let mut terminal = None;
        while let Some(frame) = self.read_frame(deadline).await? {
            if terminal.is_some() {
                return Err(match frame {
                    WorkerFrame::Result { .. } => WorkerError::Protocol(
                        "worker emitted more than one terminal result".to_string(),
                    ),
                    _ => WorkerError::Protocol(
                        "worker emitted output after its terminal result".to_string(),
                    ),
                });
            }

            match frame {
                WorkerFrame::Ready => {
                    return Err(WorkerError::Protocol(
                        "worker emitted an unexpected ready frame".to_string(),
                    ));
                }
                WorkerFrame::Event { event } => {
                    tracing::info!(?event, "worker invocation event");
                }
                WorkerFrame::Result { result } => terminal = Some(result),
            }
        }

        terminal.ok_or_else(|| {
            WorkerError::Protocol("worker stdout ended before a terminal result".to_string())
        })
    }

    async fn terminate(&self, _reason: TerminationReason) -> Result<(), WorkerError> {
        self.stdin.lock().await.take();
        let mut child = self.child.lock().await;
        let Some(process) = child.as_mut() else {
            return Ok(());
        };
        if process.try_wait().map_err(WorkerError::Process)?.is_none() {
            process.start_kill().map_err(WorkerError::Process)?;
        }
        process.wait().await.map_err(WorkerError::Process)?;
        *child = None;
        Ok(())
    }
}

impl ProcessInvocationWorker {
    async fn read_frame(&self, deadline: Instant) -> Result<Option<WorkerFrame>, WorkerError> {
        let mut line = String::new();
        let read = tokio::time::timeout_at(deadline, async {
            self.stdout.lock().await.read_line(&mut line).await
        })
        .await
        .map_err(|_| WorkerError::DeadlineExpired)?
        .map_err(WorkerError::Process)?;
        if read == 0 {
            return Ok(None);
        }
        serde_json::from_str(line.trim_end())
            .map(Some)
            .map_err(WorkerError::Deserialize)
    }
}
