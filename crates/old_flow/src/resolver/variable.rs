#[allow(unused_variables)]
pub trait VariableResolver: Send + Sync {
    fn resolve(&self, key: &str) -> Option<String>;
    fn is_secret(&self, key: &str) -> bool {
        false
    }

    fn secret_keys(&self) -> Vec<String> {
        vec![]
    }
}

pub struct ChainedResolver {
    sources: Vec<Box<dyn VariableResolver>>,
}

impl ChainedResolver {
    pub fn new(sources: Vec<Box<dyn VariableResolver>>) -> Self {
        Self { sources }
    }
}

impl VariableResolver for ChainedResolver {
    fn resolve(&self, key: &str) -> Option<String> {
        for src in &self.sources {
            if let Some(v) = src.resolve(key) {
                return Some(v);
            }
        }
        None
    }

    fn is_secret(&self, key: &str) -> bool {
        self.sources.iter().any(|s| s.is_secret(key))
    }

    fn secret_keys(&self) -> Vec<String> {
        self.sources
            .iter()
            .flat_map(|s| s.secret_keys())
            .collect::<Vec<_>>()
    }
}
