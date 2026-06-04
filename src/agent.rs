use crate::SeaportError;

/// A system that can answer evaluation prompts.
pub trait Agent {
    /// Returns a stable human-readable name for reports and telemetry.
    fn name(&self) -> &str;

    /// Produces an answer for one prompt.
    fn respond(&self, prompt: &str) -> Result<String, SeaportError>;
}

/// Agent implementation that returns the prompt unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EchoAgent {
    name: String,
}

impl EchoAgent {
    /// Creates a named echo agent.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl Agent for EchoAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn respond(&self, prompt: &str) -> Result<String, SeaportError> {
        Ok(prompt.to_owned())
    }
}

/// Agent implementation that always returns the same response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticAgent {
    name: String,
    response: String,
}

impl StaticAgent {
    /// Creates a named static-response agent.
    pub fn new(name: impl Into<String>, response: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            response: response.into(),
        }
    }
}

impl Agent for StaticAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn respond(&self, _prompt: &str) -> Result<String, SeaportError> {
        Ok(self.response.clone())
    }
}
