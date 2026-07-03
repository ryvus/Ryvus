use super::PipelineDefinition;
use crate::error::FlowError;
use std::{fs, path::Path};

pub struct PipelineLoader;

impl PipelineLoader {
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<PipelineDefinition, FlowError> {
        let data = fs::read_to_string(path).map_err(|e| FlowError::Loader(e.to_string()))?;

        let pipeline: PipelineDefinition = serde_json::from_str(&data)
            .map_err(|e| FlowError::Store(format!("JSON parse error: {}", e)))?;

        Ok(pipeline)
    }
}
