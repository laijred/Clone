use std::path::PathBuf;

use chrono::{DateTime, Utc};
use meilisearch_error::ResponseError;
use milli::update::{DocumentAdditionResult, IndexDocumentsMethod};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::{
    index::{Settings, Unchecked},
    index_resolver::{error::IndexResolverError, IndexUid},
};

use super::batch::BatchId;

pub type TaskId = u64;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskResult {
    DocumentAddition { indexed_documents: u64 },
    DocumentDeletion { deleted_documents: u64 },
    ClearAll { deleted_documents: u64 },
    Other,
}

impl From<DocumentAdditionResult> for TaskResult {
    fn from(other: DocumentAdditionResult) -> Self {
        Self::DocumentAddition {
            indexed_documents: other.indexed_documents,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskEvent {
    Created(DateTime<Utc>),
    Batched {
        timestamp: DateTime<Utc>,
        batch_id: BatchId,
    },
    Processing(DateTime<Utc>),
    Succeded {
        result: TaskResult,
        timestamp: DateTime<Utc>,
    },
    Failed {
        error: ResponseError,
        timestamp: DateTime<Utc>,
    },
}

/// A task represent an operation that Meilisearch must do.
/// It's stored on disk and executed from the lowest to higher Task id.
/// Everytime a new task is created it's have a higher Task id than the previous one.
/// See also `Job`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Task {
    pub id: TaskId,
    pub index_uid: IndexUid,
    pub content: TaskContent,
    pub events: Vec<TaskEvent>,
}

impl Task {
    /// Return true when a task is finished.
    /// A task is finished when its last state is either `Succeeded` or `Failed`.
    pub fn is_finished(&self) -> bool {
        self.events.last().map_or(false, |event| {
            matches!(event, TaskEvent::Succeded { .. } | TaskEvent::Failed { .. })
        })
    }
}

/// A job is like a volatile priority `Task`.
/// It should be processed as fast as possible and is not stored on disk.
/// This means, when Meilisearch is closed all your unprocessed jobs will disappear.
#[derive(Debug, derivative::Derivative)]
#[derivative(PartialEq)]
pub enum Job {
    Dump {
        #[derivative(PartialEq = "ignore")]
        ret: oneshot::Sender<Result<(), IndexResolverError>>,
        path: PathBuf,
    },
    // Snapshot {},
    // Task(Task),
    Empty,
}

impl Default for Job {
    fn default() -> Self {
        Self::Empty
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum DocumentDeletion {
    Clear,
    Ids(Vec<String>),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum TaskContent {
    DocumentAddition {
        content_uuid: Uuid,
        merge_strategy: IndexDocumentsMethod,
        primary_key: Option<String>,
        documents_count: usize,
    },
    DocumentDeletion(DocumentDeletion),
    SettingsUpdate {
        settings: Settings<Unchecked>,
        /// Indicates whether the task was a deletion
        is_deletion: bool,
    },
    IndexDeletion,
    IndexCreation {
        primary_key: Option<String>,
    },
    IndexUpdate {
        primary_key: Option<String>,
    },
}

#[cfg(test)]
mod test {
    use super::*;
    use quickcheck::{Arbitrary, Gen};

    impl Arbitrary for Task {
        fn arbitrary(g: &mut Gen) -> Self {
            Self {
                id: TaskId::arbitrary(g),
                index_uid: IndexUid::new_unchecked(String::arbitrary(g)),
                content: TaskContent::arbitrary(g),
                events: Vec::arbitrary(g),
            }
        }
    }

    impl Arbitrary for TaskContent {
        fn arbitrary(g: &mut Gen) -> Self {
            let rand = g.choose(&[1, 2, 3, 4]).unwrap();
            let merge_strategy = *g
                .choose(&[
                    IndexDocumentsMethod::ReplaceDocuments,
                    IndexDocumentsMethod::UpdateDocuments,
                ])
                .unwrap();
            match rand {
                1 => Self::DocumentAddition {
                    content_uuid: Uuid::new_v4(),
                    merge_strategy,
                    primary_key: Option::arbitrary(g),
                    documents_count: usize::arbitrary(g),
                },
                2 => Self::DocumentDeletion(DocumentDeletion::arbitrary(g)),
                3 => Self::IndexDeletion,
                4 => Self::SettingsUpdate {
                    settings: Settings::arbitrary(g),
                    is_deletion: bool::arbitrary(g),
                },
                _ => unreachable!(),
            }
        }
    }

    impl Arbitrary for DocumentDeletion {
        fn arbitrary(g: &mut Gen) -> Self {
            let options = &[Self::Clear, Self::Ids(Vec::arbitrary(g))];
            g.choose(options).unwrap().clone()
        }
    }

    impl Arbitrary for TaskEvent {
        fn arbitrary(g: &mut Gen) -> Self {
            let options = &[
                Self::Created(Utc::now()),
                Self::Batched {
                    timestamp: Utc::now(),
                    batch_id: BatchId::arbitrary(g),
                },
                Self::Failed {
                    timestamp: Utc::now(),
                    error: ResponseError::arbitrary(g),
                },
                Self::Succeded {
                    timestamp: Utc::now(),
                    result: TaskResult::arbitrary(g),
                },
            ];
            g.choose(options).unwrap().clone()
        }
    }

    impl Arbitrary for TaskResult {
        fn arbitrary(g: &mut Gen) -> Self {
            let n = g.choose(&[1, 2, 3]).unwrap();
            match n {
                1 => Self::Other,
                2 => Self::DocumentAddition {
                    indexed_documents: u64::arbitrary(g),
                },
                3 => Self::DocumentDeletion {
                    deleted_documents: u64::arbitrary(g),
                },
                _ => unreachable!(),
            }
        }
    }
}
