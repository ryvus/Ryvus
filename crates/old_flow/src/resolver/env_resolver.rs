use super::variable::VariableResolver;

pub struct EnvResolver;

impl VariableResolver for EnvResolver {
    fn resolve(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}
