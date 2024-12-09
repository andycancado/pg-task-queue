use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{postgres::PgListener, Pool, Postgres};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TaskNotification {
    pub operation: String,
    pub id: Uuid,
    pub name: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

type TaskHandler = Box<dyn Fn(TaskNotification) + Send + Sync>;

#[derive(Clone)]
pub struct TaskProcessor {
    pool: Pool<Postgres>,
    worker_count: usize,
    handlers: Arc<Mutex<HashMap<String, TaskHandler>>>,
}

#[derive(sqlx::Type, Debug, Clone, Copy)]
#[sqlx(type_name = "task_status", rename_all = "lowercase")]
enum TaskStatus {
    Processing,
    Completed,
    Pending,
    Failed,
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status_str = match self {
            TaskStatus::Processing => "processing",
            TaskStatus::Completed => "completed",
            TaskStatus::Pending => "pending",
            TaskStatus::Failed => "failed",
        };
        write!(f, "{}", status_str)
    }
}

impl TaskProcessor {
    pub fn new(pool: Pool<Postgres>, worker_count: usize) -> Self {
        Self {
            pool,
            worker_count,
            handlers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn register_handler<F>(&self, event: &str, handler: F)
    where
        F: Fn(TaskNotification) + Send + Sync + 'static,
    {
        let mut handlers = self.handlers.lock().await;
        handlers.insert(event.to_string(), Box::new(handler));
        tracing::info!("Handler Registered: {:?}", handlers.keys());
    }

    async fn process_task(&self, notification: TaskNotification) -> Result<(), sqlx::Error> {
        let handlers = self.handlers.lock().await;
        tracing::info!("Received task: {:?}", notification);

        if let Some(handler) = handlers.get(&notification.name) {
            tracing::info!("Processing task {}", notification.id);
            self.update_task_status(&notification.id, TaskStatus::Processing)
                .await?;
            handler(notification.clone());
            self.update_task_status(&notification.id, TaskStatus::Completed)
                .await?;
            Ok(())
        } else {
            tracing::warn!("No handler registered for task: {}", notification.id);
            self.update_task_status(&notification.id, TaskStatus::Failed)
                .await?;
            Ok(())
        }
    }

    async fn update_task_status(
        &self,
        task_id: &Uuid,
        status: TaskStatus,
    ) -> Result<(), sqlx::Error> {
        let result = sqlx::query!(
            r#"
            UPDATE tasks
            SET status = $1,
                started_at = CASE 
                    WHEN $1 = 'processing'::task_status THEN CURRENT_TIMESTAMP 
                    ELSE started_at 
                END,
                completed_at = CASE 
                    WHEN $1 = 'completed'::task_status THEN CURRENT_TIMESTAMP 
                    ELSE completed_at 
                END
            WHERE id = $2
            "#,
            status as TaskStatus,
            task_id
        )
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            tracing::error!("No task found with ID: {}", task_id);
            return Err(sqlx::Error::RowNotFound);
        }

        tracing::info!("Updated task {} status to {}", task_id, status);
        Ok(())
    }

    pub async fn start_exp(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut listener = PgListener::connect_with(&self.pool).await?;
        listener.listen("task_changes").await?;

        tracing::info!("Starting task processor with {} workers", self.worker_count);

        self.process_existing_tasks().await?;

        let (tx, rx) = mpsc::channel::<TaskNotification>(100);

        // Spawn listener task
        tokio::spawn({
            let tx = tx.clone();
            async move {
                while let Ok(notification) = listener.recv().await {
                    match serde_json::from_str::<TaskNotification>(notification.payload()) {
                        Ok(task_notif) if task_notif.status == "pending" => {
                            if let Err(e) = tx.send(task_notif).await {
                                tracing::error!("Failed to send task to channel: {}", e);
                            }
                        }
                        Ok(_) => tracing::debug!("Ignoring non-pending task"),
                        Err(e) => tracing::error!("Failed to parse notification: {}", e),
                    }
                }
            }
        });

        let rx = Arc::new(Mutex::new(rx));

        // Spawn worker tasks
        let worker_handles: Vec<_> = (0..self.worker_count)
            .map(|id| {
                let rx = rx.clone();
                let processor = self.clone();

                tokio::spawn(async move {
                    tracing::info!("Worker {} started", id);

                    loop {
                        let notification = {
                            let mut rx = rx.lock().await;
                            rx.recv().await
                        };

                        match notification {
                            Some(task) => {
                                if let Err(e) = processor.process_task(task).await {
                                    tracing::error!("Worker {} error processing task: {}", id, e);
                                }
                            }
                            None => {
                                tracing::info!("Worker {} channel closed, shutting down", id);
                                break;
                            }
                        }
                    }
                })
            })
            .collect();

        for handle in worker_handles {
            handle.await?;
        }

        Ok(())
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut listener = PgListener::connect_with(&self.pool).await?;
        listener.listen("task_changes").await?;

        tracing::info!("Starting task processor with {} workers", self.worker_count);

        self.process_existing_tasks().await?;

        let (tx, mut rx) = mpsc::channel::<TaskNotification>(100);

        tokio::spawn(async move {
            while let Ok(notification) = listener.recv().await {
                match serde_json::from_str::<TaskNotification>(notification.payload()) {
                    Ok(task_notif) if task_notif.status == "pending" => {
                        if let Err(e) = tx.send(task_notif).await {
                            tracing::error!("Failed to send task to channel: {}", e);
                        }
                    }
                    Ok(_) => tracing::debug!("Ignoring non-pending task"),
                    Err(e) => tracing::error!("Failed to parse notification: {}", e),
                }
            }
        });

        let pool = self.pool.clone();

        let mut worker_txs = Vec::new();
        let worker_handles: Vec<_> = (0..self.worker_count)
            .map(|id| {
                // for id in 0..worker_count {
                let (worker_tx, mut worker_rx) = mpsc::channel::<TaskNotification>(100);
                worker_txs.push(worker_tx);
                // let pool = pool.clone();
                let processor = self.clone();

                tokio::spawn(async move {
                    tracing::info!("Worker {} started", id);

                    while let Some(notification) = worker_rx.recv().await {

                        if let Err(e) = processor.process_task(notification).await {
                            tracing::error!("Worker {} error processing task: {}", id, e);
                        }
                    }
                })
            })
            .collect();

        // Dispatcher task
        let dispatcher = Arc::new(tokio::sync::Mutex::new(worker_txs));
        let dispatcher_clone = dispatcher.clone();
        tokio::spawn(async move {
            let mut round = 0;
            while let Some(notification) = rx.recv().await {
                let workers = dispatcher_clone.lock().await;
                if round >= workers.len() {
                    round = 0;
                }
                if let Err(e) = workers[round].send(notification).await {
                    tracing::error!("Failed to send to worker {}: {}", round, e);
                }
                round += 1;
            }
        });

        for handle in worker_handles {
            handle.await?;
        }
        Ok(())
    }

    async fn process_existing_tasks(&self) -> Result<(), sqlx::Error> {
        tracing::info!("Checking for existing pending tasks...");

        let pending_tasks = sqlx::query!(
            r#"
            SELECT id, name, created_at, status::text as "status!"
            FROM tasks 
            WHERE status = 'pending'::task_status
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        for task in pending_tasks {
            let notification = TaskNotification {
                operation: "INSERT".to_string(),
                id: task.id,
                name: task.name,
                status: task.status,
                created_at: task.created_at.unwrap_or_default(),
            };

            self.process_task(notification).await?;
        }

        Ok(())
    }
}
