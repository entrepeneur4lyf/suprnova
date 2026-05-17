//! Welcome-log job. Dispatched on user signup; just emits a tracing event.

use serde::{Deserialize, Serialize};
use suprnova::{async_trait, FrameworkError, Job};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WelcomeLog {
    pub user_id: i64,
}

#[async_trait]
impl Job for WelcomeLog {
    fn job_name() -> &'static str {
        "WelcomeLog"
    }

    async fn handle(self) -> Result<(), FrameworkError> {
        tracing::info!(user_id = self.user_id, "welcome log job ran");
        Ok(())
    }
}
