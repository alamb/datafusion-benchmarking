CREATE TABLE IF NOT EXISTS seen_comments (
    comment_id   INTEGER PRIMARY KEY,
    repo         TEXT NOT NULL,
    pr_number    INTEGER NOT NULL,
    login        TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    processed_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS benchmark_jobs (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    comment_id     INTEGER NOT NULL REFERENCES seen_comments(comment_id),
    repo           TEXT NOT NULL,
    pr_number      INTEGER NOT NULL,
    pr_url         TEXT NOT NULL,
    login          TEXT NOT NULL,
    benchmarks     TEXT NOT NULL,         -- JSON array of benchmark names
    env_vars       TEXT NOT NULL DEFAULT '[]', -- JSON array of KEY=VALUE strings
    job_type       TEXT NOT NULL,         -- standard | criterion | arrow_criterion
    cpu_request    TEXT,
    memory_request TEXT,
    cpu_arch       TEXT,
    k8s_job_name   TEXT,
    status         TEXT NOT NULL DEFAULT 'pending',
    error_message  TEXT,
    created_at     TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at     TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS scan_state (
    repo         TEXT PRIMARY KEY,
    last_scan_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_jobs_status ON benchmark_jobs(status);
CREATE INDEX IF NOT EXISTS idx_jobs_k8s ON benchmark_jobs(k8s_job_name);
