CREATE TABLE project (
    id TEXT PRIMARY KEY,
    worktree TEXT NOT NULL,
    name TEXT
);

CREATE TABLE session (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    directory TEXT NOT NULL,
    title TEXT NOT NULL,
    version TEXT,
    FOREIGN KEY (project_id) REFERENCES project(id)
);

CREATE TABLE message (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    time_created INTEGER NOT NULL,
    data TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES session(id)
);

INSERT INTO project (id, worktree, name)
VALUES ('project-a', '/work/project-a', 'Project A');

INSERT INTO session (id, project_id, directory, title, version)
VALUES ('session-a', 'project-a', '/work/project-a', 'Ancient session', '0.5.0');

INSERT INTO message (id, session_id, time_created, data)
VALUES (
    'assistant-a',
    'session-a',
    1000,
    '{"role":"assistant","cost":0.5,"tokens":{"input":1,"output":2},"modelID":"old-model","providerID":"old-provider"}'
);
