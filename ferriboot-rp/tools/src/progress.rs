//! Translate probe-rs 0.32's real erase/program/verify events into terminal
//! progress bars. In particular, do not count elapsed time as erased bytes.
use ferriboot_build::tools::progress::Progress;
use probe_rs::flashing::{ProgressEvent, ProgressOperation};

pub(crate) struct ProbeProgress {
    preparing: Option<Progress>,
    totals: [Option<u64>; 4],
    active: Option<(usize, Progress)>,
}

impl ProbeProgress {
    pub(crate) fn new(preparing: &str) -> Self {
        Self {
            preparing: Some(Progress::new(preparing, None)),
            totals: [None; 4],
            active: None,
        }
    }

    pub(crate) fn event(&mut self, event: ProgressEvent) {
        match event {
            ProgressEvent::AddProgressBar { operation, total } => {
                self.totals[index(operation)] = total;
            }
            ProgressEvent::Started(operation) => {
                if let Some(mut preparing) = self.preparing.take() {
                    preparing.finish();
                }
                // Normally events are sequential, but do not leave a previous
                // progress bar spinning if a backend moves to another phase.
                self.active = Some((
                    index(operation),
                    Progress::new(label(operation), self.totals[index(operation)]),
                ));
            }
            ProgressEvent::Progress {
                operation, size, ..
            } => {
                if let Some((active_op, progress)) = &mut self.active
                    && *active_op == index(operation)
                {
                    progress.inc(size);
                }
            }
            ProgressEvent::Finished(operation) => {
                if self.active.as_ref().map(|(op, _)| *op) == Some(index(operation))
                    && let Some((_, mut progress)) = self.active.take()
                {
                    progress.finish();
                }
            }
            ProgressEvent::Failed(operation) => {
                if self.active.as_ref().map(|(op, _)| *op) == Some(index(operation)) {
                    // Drop displays failed, never 100%.
                    drop(self.active.take());
                }
            }
            ProgressEvent::DiagnosticMessage { message } => eprintln!("probe-rs: {message}"),
            ProgressEvent::FlashLayoutReady { .. } => {}
        }
    }

    pub(crate) fn finish(mut self) {
        if let Some(mut pending) = self.preparing.take() {
            pending.finish();
        }
        if let Some((_, mut active)) = self.active.take() {
            active.finish();
        }
    }
}

fn index(operation: ProgressOperation) -> usize {
    match operation {
        ProgressOperation::Fill => 0,
        ProgressOperation::Erase => 1,
        ProgressOperation::Program => 2,
        ProgressOperation::Verify => 3,
    }
}
fn label(operation: ProgressOperation) -> &'static str {
    match operation {
        ProgressOperation::Fill => "Reading flash to preserve data",
        ProgressOperation::Erase => "Erasing flash",
        ProgressOperation::Program => "Programming flash",
        ProgressOperation::Verify => "Verifying flash",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn consumes_all_probe_rs_progress_operations() {
        let mut reporter = ProbeProgress::new("Preparing flash");
        for op in [
            ProgressOperation::Fill,
            ProgressOperation::Erase,
            ProgressOperation::Program,
            ProgressOperation::Verify,
        ] {
            reporter.event(ProgressEvent::AddProgressBar {
                operation: op,
                total: Some(4096),
            });
            reporter.event(ProgressEvent::Started(op));
            reporter.event(ProgressEvent::Progress {
                operation: op,
                size: 4096,
                time: Duration::ZERO,
            });
            reporter.event(ProgressEvent::Finished(op));
        }
        reporter.finish();
    }
}
