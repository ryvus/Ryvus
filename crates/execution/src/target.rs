use std::{collections::HashMap, path::PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeTarget {
    LocalProcess(LocalProcessTarget),
    Http { endpoint: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalProcessTarget {
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<PathBuf>,
    pub env: HashMap<String, String>,
    pub source: Option<PathBuf>,
}

impl RuntimeTarget {
    pub fn local_process(command: impl Into<String>) -> Self {
        Self::LocalProcess(LocalProcessTarget {
            command: command.into(),
            args: Vec::new(),
            working_dir: None,
            env: HashMap::new(),
            source: None,
        })
    }

    pub fn http(endpoint: impl Into<String>) -> Self {
        Self::Http {
            endpoint: endpoint.into(),
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        if let Self::LocalProcess(target) = &mut self {
            target.args.push(arg.into());
        }
        self
    }

    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        if let Self::LocalProcess(target) = &mut self {
            target.args.extend(args.into_iter().map(Into::into));
        }
        self
    }

    pub fn working_dir(mut self, working_dir: impl Into<PathBuf>) -> Self {
        if let Self::LocalProcess(target) = &mut self {
            target.working_dir = Some(working_dir.into());
        }
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        if let Self::LocalProcess(target) = &mut self {
            target.env.insert(key.into(), value.into());
        }
        self
    }

    pub fn source(mut self, source: impl Into<PathBuf>) -> Self {
        if let Self::LocalProcess(target) = &mut self {
            target.source = Some(source.into());
        }
        self
    }
}
