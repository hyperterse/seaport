use std::error::Error;
use std::fmt;

/// High-level category for a runtime error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Input data or public API usage failed validation.
    Validation,
    /// Agent execution failed.
    Agent,
    /// Scoring produced an invalid value.
    Scoring,
    /// Runtime configuration is not usable.
    Configuration,
}

/// Structured error type used throughout Runtime.
#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeError {
    /// A test case ID was empty after trimming whitespace.
    EmptyCaseId,
    /// Two test cases used the same ID.
    DuplicateCaseId { case_id: String },
    /// The agent name was empty after trimming whitespace.
    EmptyAgentName,
    /// An agent failed while answering a case.
    AgentFailed {
        agent: String,
        case_id: Option<String>,
        message: String,
    },
    /// A scorer returned a value outside the supported `0.0..=1.0` range.
    InvalidScore {
        scorer: String,
        case_id: String,
        score: f64,
    },
    /// An agent response exceeded the configured output limit.
    OutputTooLong {
        case_id: String,
        limit: usize,
        actual: usize,
    },
    /// A configuration value failed validation.
    InvalidConfig { message: String },
}

impl RuntimeError {
    /// Returns a stable category for routing and metrics.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::EmptyCaseId | Self::DuplicateCaseId { .. } | Self::EmptyAgentName => {
                ErrorKind::Validation
            }
            Self::AgentFailed { .. } | Self::OutputTooLong { .. } => ErrorKind::Agent,
            Self::InvalidScore { .. } => ErrorKind::Scoring,
            Self::InvalidConfig { .. } => ErrorKind::Configuration,
        }
    }

    /// Returns a stable machine-readable error code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::EmptyCaseId => "runtime.validation.empty_case_id",
            Self::DuplicateCaseId { .. } => "runtime.validation.duplicate_case_id",
            Self::EmptyAgentName => "runtime.validation.empty_agent_name",
            Self::AgentFailed { .. } => "runtime.agent.failed",
            Self::InvalidScore { .. } => "runtime.scoring.invalid_score",
            Self::OutputTooLong { .. } => "runtime.agent.output_too_long",
            Self::InvalidConfig { .. } => "runtime.config.invalid",
        }
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCaseId => write!(formatter, "test case ID cannot be empty"),
            Self::DuplicateCaseId { case_id } => {
                write!(formatter, "duplicate test case ID: {case_id}")
            }
            Self::EmptyAgentName => write!(formatter, "agent name cannot be empty"),
            Self::AgentFailed {
                agent,
                case_id,
                message,
            } => match case_id {
                Some(case_id) => write!(
                    formatter,
                    "agent {agent} failed while evaluating case {case_id}: {message}"
                ),
                None => write!(formatter, "agent {agent} failed: {message}"),
            },
            Self::InvalidScore {
                scorer,
                case_id,
                score,
            } => write!(
                formatter,
                "scorer {scorer} returned invalid score {score} for case {case_id}"
            ),
            Self::OutputTooLong {
                case_id,
                limit,
                actual,
            } => write!(
                formatter,
                "case {case_id} produced {actual} characters, exceeding limit {limit}"
            ),
            Self::InvalidConfig { message } => write!(formatter, "invalid config: {message}"),
        }
    }
}

impl Error for RuntimeError {}
