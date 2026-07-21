CREATE TABLE schema_version (
    version INTEGER NOT NULL
);

INSERT INTO schema_version (version) VALUES (6);

CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    cwd TEXT,
    repository TEXT,
    host_type TEXT,
    branch TEXT,
    summary TEXT,
    created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now'))
);

CREATE TABLE assistant_usage_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    turn_index INTEGER,
    agent_id TEXT,
    parent_tool_call_id TEXT,
    model TEXT NOT NULL,
    input_tokens INTEGER,
    output_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    reasoning_tokens INTEGER,
    total_nano_aiu INTEGER,
    request_multiplier REAL,
    duration_ms INTEGER,
    time_to_first_token_ms INTEGER,
    inter_token_latency_ms INTEGER,
    initiator TEXT,
    api_endpoint TEXT,
    reasoning_effort TEXT,
    finish_reason TEXT,
    content_filter_triggered INTEGER,
    token_details_json TEXT,
    created_at TEXT DEFAULT (datetime('now'))
);

INSERT INTO sessions (id, cwd, repository, host_type, branch)
VALUES ('session-a', '/work/copilot-project', 'owner/copilot-project', 'github', 'main');

INSERT INTO assistant_usage_events (
    session_id,
    turn_index,
    model,
    input_tokens,
    output_tokens,
    cache_read_tokens,
    cache_write_tokens,
    reasoning_tokens,
    total_nano_aiu,
    reasoning_effort,
    token_details_json,
    created_at
) VALUES (
    'session-a',
    0,
    'gpt-5.6-terra',
    15998,
    5,
    0,
    0,
    0,
    5006687500,
    'high',
    '[{"batchSize":1000000,"costPerBatch":250000000000,"tokenCount":3,"tokenType":"input"},{"batchSize":1000000,"costPerBatch":25000000000,"tokenCount":0,"tokenType":"cache_read"},{"batchSize":1000000,"costPerBatch":312500000000,"tokenCount":15995,"tokenType":"cache_write"},{"batchSize":1000000,"costPerBatch":1500000000000,"tokenCount":5,"tokenType":"output"}]',
    '2026-07-21T08:59:08.169Z'
);
