-- Per-side configuration: independent env vars and custom refs for baseline vs changed.
ALTER TABLE benchmark_jobs ADD COLUMN baseline_env_vars TEXT NOT NULL DEFAULT '{}';
ALTER TABLE benchmark_jobs ADD COLUMN changed_env_vars TEXT NOT NULL DEFAULT '{}';
ALTER TABLE benchmark_jobs ADD COLUMN baseline_ref TEXT;
ALTER TABLE benchmark_jobs ADD COLUMN changed_ref TEXT;
