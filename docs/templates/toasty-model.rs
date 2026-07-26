use serde_json::Value;
use toasty_mgr::{Deferred, Embed, Executor, Model, ModelSet};

/// A value object stored as one JSON document while retaining typed query paths.
#[derive(Debug, Embed)]
pub struct WorkspaceProfile {
    pub region: String,
    pub retention_days: i64,
}

/// Stable numeric codes are useful only when they are part of the data contract.
#[derive(Debug, Embed)]
#[column(type = u8)]
pub enum TaskPriority {
    #[column(variant = 10)]
    Low,
    #[column(variant = 20)]
    Normal,
    #[column(variant = 30)]
    High,
}

/// Different variants share one logical address column and one unique index.
#[derive(Debug, Embed)]
#[unique(address)]
pub enum NotificationTarget {
    #[column(variant = 1)]
    Email {
        #[shared(address)]
        email: String,
    },
    #[column(variant = 2)]
    Webhook {
        #[shared(address)]
        url: String,
        secret_reference: String,
    },
}

#[derive(Debug, Model)]
#[table = "workspaces"]
pub struct Workspace {
    #[key]
    #[auto]
    pub id: i64,

    /// Generates indexed lookup and `upsert_by_slug`.
    #[unique]
    pub slug: String,

    pub display_name: String,

    /// Toasty 0.9 requires an explicit storage type for every JSON field.
    #[column(type = jsonb)]
    pub settings: Value,

    #[document]
    pub profile: WorkspaceProfile,

    pub notification: NotificationTarget,

    #[has_many]
    pub tasks: Deferred<Vec<Task>>,
}

#[derive(Debug, Model)]
#[table = "tasks"]
pub struct Task {
    #[key]
    #[auto]
    pub id: i64,

    /// Optional so a direct relation `remove()` can clear the foreign key.
    #[index]
    pub workspace_id: Option<i64>,

    /// Toasty infers `workspace_id -> Workspace::id` from the field names.
    #[belongs_to]
    pub workspace: Deferred<Option<Workspace>>,

    /// Generates indexed lookup and `upsert_by_external_id`.
    #[unique]
    pub external_id: String,

    pub title: String,

    #[index]
    pub priority: TaskPriority,

    #[default(false)]
    pub completed: bool,

    #[column(type = jsonb)]
    pub payload: Value,
}

/// Register the complete model graph for one managed data source.
pub fn model_set() -> ModelSet {
    toasty_mgr::models!(Workspace, Task)
}

/// Toasty 0.9 upserts by a primary key or unique constraint.
pub async fn upsert_task(
    executor: &mut dyn Executor,
    external_id: &str,
    title: &str,
    priority: TaskPriority,
    payload: Value,
) -> toasty_mgr::Result<Task> {
    Task::upsert_by_external_id(external_id)
        .title(title)
        .priority(priority)
        .payload(payload)
        .exec(executor)
        .await
}

/// Filter and order the tasks loaded into each workspace relation.
pub async fn load_open_tasks(executor: &mut dyn Executor) -> toasty_mgr::Result<Vec<Workspace>> {
    Workspace::all()
        .include(
            Workspace::fields()
                .tasks()
                .filter(Task::fields().completed().eq(false))
                .order_by(Task::fields().id().asc()),
        )
        .exec(executor)
        .await
}

/// Query through a nested path even though `profile` occupies one document column.
pub async fn workspaces_in_region(
    executor: &mut dyn Executor,
    region: &str,
) -> toasty_mgr::Result<Vec<Workspace>> {
    Workspace::filter(Workspace::fields().profile().region().eq(region))
        .exec(executor)
        .await
}

/// Relation mutation is lazy in Toasty 0.9 and requires explicit execution.
pub async fn attach_task(
    executor: &mut dyn Executor,
    workspace: &Workspace,
    task: &Task,
) -> toasty_mgr::Result<()> {
    workspace.tasks().insert(task).exec(executor).await
}

pub async fn detach_task(
    executor: &mut dyn Executor,
    workspace: &Workspace,
    task: &Task,
) -> toasty_mgr::Result<()> {
    workspace.tasks().remove(task).exec(executor).await
}
