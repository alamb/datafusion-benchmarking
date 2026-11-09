-- The raw trigger comment, quoted in result comments. NULL for rows queued
-- before this migration.
ALTER TABLE benchmark_jobs ADD COLUMN trigger_comment TEXT;
