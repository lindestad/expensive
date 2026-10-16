CREATE TABLE project (
    id TEXT PRIMARY KEY,
    worktree TEXT NOT NULL,
    name TEXT
);

CREATE TABLE session_v2 (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    title TEXT,
    FOREIGN KEY (project_id) REFERENCES project(id)
);

CREATE TABLE session_message (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    type TEXT NOT NULL,
    seq INTEGER NOT NULL,
    time_created INTEGER NOT NULL,
    time_updated INTEGER NOT NULL,
    data TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES session_v2(id)
);

INSERT INTO project (id, worktree, name)
VALUES ('project-a', '/work/project-a', 'Project A');

INSERT INTO session_v2 (id, project_id, title)
VALUES ('session-a', 'project-a', 'Modern session');

INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data)
VALUES (
    'assistant-a',
    'session-a',
    'assistant',
    1,
    1000,
    1000,
    '{"cost":1.25,"tokens":{"input":10,"output":20},"model":{"id":"gpt-test","providerID":"github-copilot","variant":"default"}}'
);
