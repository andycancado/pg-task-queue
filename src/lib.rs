use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{postgres::PgListener, Pool, Postgres};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TaskNotification {
    pub operation: String,
    pub id: Uuid,
    pub name: String,
    pub status: String,
    pub payload: Option<Value>,
    pub created_at: DateTime<Utc>,
}

#[async_trait]
pub trait TaskHandler: Send + Sync {
    async fn handle(&self, task: TaskNotification) -> Result<(), sqlx::Error>;
}

type DynTaskHandler = Arc<dyn TaskHandler>;

#[derive(Clone)]
pub struct TaskProcessor {
    pool: Pool<Postgres>,
    worker_count: usize,
    handlers: Arc<Mutex<HashMap<String, DynTaskHandler>>>,
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

    pub async fn register_handler<H>(&self, event: &str, handler: H)
    where
        H: TaskHandler + 'static,
    {
        let mut handlers = self.handlers.lock().await;
        handlers.insert(event.to_string(), Arc::new(handler));
        tracing::info!("Handler Registered: {:?}", handlers.keys());
    }

    async fn process_task(&self, notification: TaskNotification) -> Result<(), sqlx::Error> {
        let handlers = self.handlers.lock().await;
        tracing::info!("Received task: {:?}", notification);

        if let Some(handler) = handlers.get(&notification.name) {
            tracing::info!("Processing task {}", notification.id);
            self.update_task_status(&notification.id, TaskStatus::Processing)
                .await?;

            let handler = handler.clone();
            let notification_clone = notification.clone();
            let pool = self.pool.clone();

            tokio::spawn(async move {
                let task_status = match handler.handle(notification_clone).await {
                    Ok(_) => TaskStatus::Completed,
                    Err(e) => {
                        tracing::error!("Task handler error: {}", e);
                        TaskStatus::Failed
                    }
                };

                if let Err(e) =
                    Self::update_task_status_static(&pool, &notification.id, task_status).await
                {
                    tracing::error!("Failed to update task status: {}", e);
                }
            });

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
        Self::update_task_status_static(&self.pool, task_id, status).await
    }

    async fn update_task_status_static(
        pool: &Pool<Postgres>,
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
        .execute(pool)
        .await?;

        if result.rows_affected() == 0 {
            tracing::error!("No task found with ID: {}", task_id);
            return Err(sqlx::Error::RowNotFound);
        }

        tracing::info!("Updated task {} status to {}", task_id, status);
        Ok(())
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut listener = PgListener::connect_with(&self.pool).await?;
        listener.listen("task_changes").await?;

        tracing::info!("Starting task processor with {} workers", self.worker_count);

        self.process_existing_tasks().await?;

        let (tx, rx) = mpsc::channel::<TaskNotification>(100);
        let rx = Arc::new(Mutex::new(rx));

        let tx_clone = tx.clone();
        tokio::spawn(async move {
            while let Ok(notification) = listener.recv().await {
                match serde_json::from_str::<TaskNotification>(notification.payload()) {
                    Ok(task_notif) if task_notif.status == "pending" => {
                        if let Err(e) = tx_clone.send(task_notif).await {
                            tracing::error!("Failed to send task to channel: {}", e);
                        }
                    }
                    Ok(_) => tracing::debug!("Ignoring non-pending task"),
                    Err(e) => tracing::error!("Failed to parse notification: {}", e),
                }
            }
        });

        // Spawn worker tasks
        let worker_handles: Vec<_> = (0..self.worker_count)
            .map(|id| {
                let processor = self.clone();
                let rx = rx.clone();

                tokio::spawn(async move {
                    tracing::info!("Worker {} started", id);
                    loop {
                        let notification = {
                            let mut rx_lock = rx.lock().await;
                            rx_lock.recv().await
                        };

                        match notification {
                            Some(task) => {
                                let processor_clone = processor.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = processor_clone.process_task(task).await {
                                        tracing::error!(
                                            "Worker {} error processing task: {}",
                                            id,
                                            e
                                        );
                                    }
                                });
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

    async fn process_existing_tasks(&self) -> Result<(), sqlx::Error> {
        tracing::info!("Checking for existing pending tasks...");

        let pending_tasks = sqlx::query!(
            r#"
            SELECT id, name, payload, created_at, status::text as "status!"
            FROM tasks 
            WHERE status = 'pending'::task_status
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        let futures = pending_tasks.into_iter().map(|task| {
            let notification = TaskNotification {
                operation: "INSERT".to_string(),
                id: task.id,
                name: task.name,
                status: task.status,
                payload: task.payload,
                created_at: task.created_at.unwrap_or_default(),
            };
            self.process_task(notification)
        });

        futures::future::join_all(futures)
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;

        Ok(())
    }
}
