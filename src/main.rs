use std::{thread, time};

use pg_task_processor::TaskProcessor;
use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let database_url = "postgres://taskuser:taskpass@localhost:5433/taskdb";

    // Create connection pool
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await?;

    let processor = TaskProcessor::new(pool.clone(), 5);

    // Register a handler for the "INSERT" event
    processor
        .register_handler("print_test", |notification| {
            println!(" MAIN: Handling INSERT event for task: {}", notification.id);
            thread::sleep(time::Duration::from_secs(5));
        })
        .await;

    processor.start().await?;

    Ok(())
}
