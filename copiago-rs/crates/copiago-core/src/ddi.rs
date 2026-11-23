//! Progress shared by the developer image installer and its consumers.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DdiStage {
    Download,
    Sign,
    Install,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub received_bytes: u64,
    /// A missing total indicates indeterminate progress.
    pub total_bytes: Option<u64>,
    pub bytes_per_second: u64,
}
