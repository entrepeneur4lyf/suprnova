//! Todo actions

use suprnova::database::{EntityExt, EntityExtMut};
use suprnova::injectable;
use sea_orm::Set;

use crate::models::todos;

#[injectable]
pub struct CreateRandomTodoAction;

impl CreateRandomTodoAction {
    pub async fn execute(&self) -> Result<todos::Model, suprnova::error::FrameworkError> {
        let random_num = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
            % 10000;

        let new_todo = todos::ActiveModel {
            title: Set(format!("Todo #{}", random_num)),
            description: Set(Some(format!(
                "This is a random todo created at timestamp {}",
                random_num
            ))),
            ..Default::default()
        };

        todos::Entity::insert_one(new_todo).await
    }
}

#[injectable]
pub struct ListTodosAction;

impl ListTodosAction {
    pub async fn execute(&self) -> Result<Vec<todos::Model>, suprnova::error::FrameworkError> {
        todos::Entity::all().await
    }
}
