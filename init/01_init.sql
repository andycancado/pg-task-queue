-- Enable required extensions
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

-- Create enum for task status
DO $$ BEGIN
    CREATE TYPE task_status AS ENUM ('pending', 'processing', 'completed', 'failed');
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

-- Create tasks table
CREATE TABLE IF NOT EXISTS tasks (
    id UUID DEFAULT uuid_generate_v4() PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    payload JSONB,
    status task_status DEFAULT 'pending'::task_status,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    started_at TIMESTAMP WITH TIME ZONE,
    completed_at TIMESTAMP WITH TIME ZONE,
    error_message TEXT
);

-- Create index on status for faster queries
CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);

-- Function to update updated_at timestamp
CREATE OR REPLACE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ language 'plpgsql';

-- Trigger to automatically update updated_at
DROP TRIGGER IF EXISTS update_tasks_updated_at ON tasks;
CREATE TRIGGER update_tasks_updated_at
    BEFORE UPDATE ON tasks
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();

-- Create notification function
CREATE OR REPLACE FUNCTION notify_task_changes()
RETURNS trigger AS $$
DECLARE
    payload json;
BEGIN
    payload := json_build_object(
        'operation', TG_OP,
        'id', NEW.id,
        'name', NEW.name,
        'status', NEW.status::text,
        'created_at', NEW.created_at
    );
    
    PERFORM pg_notify('task_changes', payload::text);
    
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- Create notification trigger
DROP TRIGGER IF EXISTS task_changes_trigger ON tasks;
CREATE TRIGGER task_changes_trigger
    AFTER INSERT OR UPDATE ON tasks
    FOR EACH ROW
    EXECUTE FUNCTION notify_task_changes();

-- Function to check task status
CREATE OR REPLACE FUNCTION check_task_status(task_id UUID)
RETURNS TABLE (
    task_id UUID,
    task_name VARCHAR,
    task_status task_status,
    task_created_at TIMESTAMP WITH TIME ZONE,
    task_started_at TIMESTAMP WITH TIME ZONE,
    task_completed_at TIMESTAMP WITH TIME ZONE
) AS $$
BEGIN
    RETURN QUERY
    SELECT 
        t.id,
        t.name,
        t.status,
        t.created_at,
        t.started_at,
        t.completed_at
    FROM tasks t
    WHERE t.id = task_id;
END;
$$ LANGUAGE plpgsql;

-- Create helper function to insert test tasks
CREATE OR REPLACE FUNCTION create_test_task(
    task_name VARCHAR DEFAULT 'test_task',
    task_payload JSONB DEFAULT '{"test": true}'::jsonb
) RETURNS UUID AS $$
DECLARE
    new_task_id UUID;
BEGIN
    INSERT INTO tasks (name, payload)
    VALUES (task_name, task_payload)
    RETURNING id INTO new_task_id;
    
    RETURN new_task_id;
END;
$$ LANGUAGE plpgsql;
