/// Severity attached to a telemetry event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryLevel {
    /// Normal lifecycle event.
    Info,
    /// Failure event.
    Error,
}

/// Deterministic event emitted during evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryEvent {
    /// Monotonic sequence number, starting at one for each run.
    pub sequence: u64,
    /// Event severity.
    pub level: TelemetryLevel,
    /// Stable event name.
    pub name: String,
    /// Sorted key-value attributes.
    pub attributes: Vec<(String, String)>,
}

/// In-memory telemetry recorder used by the evaluator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryRecorder {
    events: Vec<TelemetryEvent>,
    next_sequence: u64,
}

impl Default for TelemetryRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetryRecorder {
    /// Creates an empty recorder.
    pub fn new() -> Self {
        Self {
            events: Vec::new(),
            next_sequence: 1,
        }
    }

    /// Records an info-level event.
    pub fn info(&mut self, name: impl Into<String>, attributes: Vec<(String, String)>) {
        self.record(TelemetryLevel::Info, name, attributes);
    }

    /// Records an error-level event.
    pub fn error(&mut self, name: impl Into<String>, attributes: Vec<(String, String)>) {
        self.record(TelemetryLevel::Error, name, attributes);
    }

    /// Returns the events recorded so far.
    pub fn events(&self) -> &[TelemetryEvent] {
        &self.events
    }

    /// Consumes the recorder and returns its events.
    pub fn into_events(self) -> Vec<TelemetryEvent> {
        self.events
    }

    fn record(
        &mut self,
        level: TelemetryLevel,
        name: impl Into<String>,
        attributes: Vec<(String, String)>,
    ) {
        let event = TelemetryEvent {
            sequence: self.next_sequence,
            level,
            name: name.into(),
            attributes: sorted_attributes(attributes),
        };

        self.events.push(event);
        self.next_sequence += 1;
    }
}

/// Builds deterministic telemetry attributes from an array of string pairs.
pub fn telemetry_attributes<const N: usize>(pairs: [(&str, String); N]) -> Vec<(String, String)> {
    sorted_attributes(
        pairs
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

fn sorted_attributes(mut attributes: Vec<(String, String)>) -> Vec<(String, String)> {
    attributes.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    attributes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_sequences_start_at_one() {
        let mut recorder = TelemetryRecorder::new();

        recorder.info("first", Vec::new());
        recorder.error("second", Vec::new());

        let sequences = recorder
            .events()
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>();

        assert_eq!(sequences, vec![1, 2]);
    }

    #[test]
    fn telemetry_attributes_are_sorted() {
        let attributes = telemetry_attributes([
            ("zeta", "last".to_owned()),
            ("alpha", "first".to_owned()),
        ]);

        assert_eq!(
            attributes,
            vec![
                ("alpha".to_owned(), "first".to_owned()),
                ("zeta".to_owned(), "last".to_owned())
            ]
        );
    }
}
