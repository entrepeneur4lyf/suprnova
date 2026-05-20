//! Todo actions
//!
//! Phase 10A T11 — these actions now go through the migrated
//! `#[suprnova::model]` `Todo` type. `Todo::create(attrs!{...})` covers
//! the create path, `Todo::all()` covers the listing path; the old
//! `EntityExt` / `EntityExtMut` direct entity calls are gone.

use suprnova::attrs;
use suprnova::injectable;
use suprnova::Model;

use crate::models::todos::Todo;

#[injectable]
pub struct CreateRandomTodoAction;

impl CreateRandomTodoAction {
    pub async fn execute(&self) -> Result<Todo, suprnova::error::FrameworkError> {
        let random_num = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
            % 10000;

        Todo::create(attrs! {
            title: format!("Todo #{}", random_num),
            description: format!("This is a random todo created at timestamp {}", random_num),
            done: false,
        })
        .await
    }
}

#[injectable]
pub struct ListTodosAction;

impl ListTodosAction {
    pub async fn execute(&self) -> Result<Vec<Todo>, suprnova::error::FrameworkError> {
        <Todo as suprnova::eloquent::Model>::all().await
    }
}
