-- Per-job markdown section body, written by the runner each time the job's
-- visible state on the trigger comment changes (running → completed/failed).
-- The controller renders the trigger comment by concatenating section_body
-- across all jobs that share a comment_id.
ALTER TABLE benchmark_jobs ADD COLUMN section_body TEXT;
