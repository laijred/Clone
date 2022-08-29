use meilisearch_types::error::{Code, ErrorCode};
use meilisearch_types::internal_error;
use tokio::task::JoinError;

use crate::update_file_store::UpdateFileStoreError;

use super::task::TaskId;

pub type Result<T> = std::result::Result<T, TaskError>;

#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error("Task `{0}` not found.")]
    UnexistingTask(TaskId),
    #[error("Task `{0}` cannot be cancel as it is already finished or being processed.")]
    InvalidTaskCancellation(TaskId),
    #[error("Internal error: {0}")]
    Internal(Box<dyn std::error::Error + Send + Sync + 'static>),
}

internal_error!(
    TaskError: milli::heed::Error,
    JoinError,
    std::io::Error,
    serde_json::Error,
    UpdateFileStoreError
);

impl ErrorCode for TaskError {
    fn error_code(&self) -> Code {
        match self {
            TaskError::UnexistingTask(_) => Code::TaskNotFound,
            TaskError::InvalidTaskCancellation(_) => Code::InvalidTaskCancellation,
            TaskError::Internal(_) => Code::Internal,
        }
    }
}
