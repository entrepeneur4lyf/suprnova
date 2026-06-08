//! SeaORM entities for workflows

/// SeaORM entity for the `workflows` table.
///
/// One row per orchestrated workflow run, holding the durable input, latest
/// output/error, attempt counters, and the worker lease that prevents two
/// workers from picking up the same workflow concurrently.
pub mod workflows {
    use sea_orm::entity::prelude::*;

    /// Row in the `workflows` table.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "workflows")]
    pub struct Model {
        /// Primary key.
        #[sea_orm(primary_key)]
        pub id: i64,
        /// Symbolic name of the registered workflow (its `#[workflow]` ident).
        pub name: String,
        /// Workflow lifecycle state (`pending`, `running`, `completed`, `failed`).
        pub status: String,
        /// Serialised workflow input (JSON).
        #[sea_orm(column_type = "Text")]
        pub input: String,
        /// Serialised workflow output once execution completes (JSON).
        #[sea_orm(column_type = "Text", nullable)]
        pub output: Option<String>,
        /// Terminal error message if the workflow failed.
        #[sea_orm(column_type = "Text", nullable)]
        pub error: Option<String>,
        /// Number of times this workflow has been attempted.
        pub attempts: i32,
        /// Maximum number of attempts before the workflow is marked failed.
        pub max_attempts: i32,
        /// Earliest UTC instant at which the workflow is eligible for pickup.
        pub next_run_at: Option<chrono::NaiveDateTime>,
        /// Lease deadline; workers ignore rows whose lease has not yet expired.
        pub locked_until: Option<chrono::NaiveDateTime>,
        /// Identifier of the worker currently holding the lease, if any.
        pub worker_id: Option<String>,
        /// Timestamp at which the row was inserted.
        pub created_at: chrono::NaiveDateTime,
        /// Timestamp at which the row was last mutated.
        pub updated_at: chrono::NaiveDateTime,
        /// Timestamp at which execution began, if started.
        pub started_at: Option<chrono::NaiveDateTime>,
        /// Timestamp at which execution finished, if completed.
        pub completed_at: Option<chrono::NaiveDateTime>,
    }

    /// SeaORM relation set for `workflows` (no relations currently exposed).
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// SeaORM entity for the `workflow_steps` table.
///
/// One row per `#[step]` invocation inside a parent workflow, capturing the
/// step's serialised input/output, status, and timing.
pub mod workflow_steps {
    use sea_orm::entity::prelude::*;

    /// Row in the `workflow_steps` table.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "workflow_steps")]
    pub struct Model {
        /// Primary key.
        #[sea_orm(primary_key)]
        pub id: i64,
        /// Foreign key into the parent `workflows.id`.
        pub workflow_id: i64,
        /// Zero-based ordinal position of this step within its workflow.
        pub step_index: i32,
        /// Symbolic name of the step (the `#[step]` function name).
        pub step_name: String,
        /// Step lifecycle state (`pending`, `running`, `completed`, `failed`).
        pub status: String,
        /// Serialised step input (JSON).
        #[sea_orm(column_type = "Text")]
        pub input: String,
        /// Serialised step output once the step completes (JSON).
        #[sea_orm(column_type = "Text", nullable)]
        pub output: Option<String>,
        /// Error message captured if the step failed.
        #[sea_orm(column_type = "Text", nullable)]
        pub error: Option<String>,
        /// Number of times this step has been attempted.
        pub attempts: i32,
        /// Timestamp at which the row was inserted.
        pub created_at: chrono::NaiveDateTime,
        /// Timestamp at which the row was last mutated.
        pub updated_at: chrono::NaiveDateTime,
        /// Timestamp at which execution began, if started.
        pub started_at: Option<chrono::NaiveDateTime>,
        /// Timestamp at which execution finished, if completed.
        pub completed_at: Option<chrono::NaiveDateTime>,
    }

    /// SeaORM relation set for `workflow_steps` (no relations currently exposed).
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
