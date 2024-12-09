use async_trait::async_trait;
use tokio::time::{sleep, Duration};

use pg_task_processor::{TaskHandler, TaskNotification, TaskProcessor};
use sqlx::postgres::PgPoolOptions;

struct PrintTaskHandler;

#[async_trait]
impl TaskHandler for PrintTaskHandler {
    async fn handle(&self, task: TaskNotification) -> Result<(), sqlx::Error> {
        println!("HANDLER: Handling INSERT event for task: {:?}", task);
        sleep(Duration::from_secs(5)).await;
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let database_url = "postgres://taskuser:taskpass@localhost:5433/taskdb";

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await?;

    let processor = TaskProcessor::new(pool, 4);
    processor
        .register_handler("my_task", PrintTaskHandler{})
        .await;
    processor
        .register_handler("my_task1", PrintTaskHandler{})
        .await;

    processor.start().await?;
    Ok(())
}
