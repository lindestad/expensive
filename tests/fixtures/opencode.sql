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
    time_updated INTEGER NOT NULL,
    data TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES session(id)
);

INSERT INTO project (id, worktree, name)
VALUES ('project-a', '/work/project-a', 'Project A');

INSERT INTO session (id, project_id, directory, title, version)
VALUES ('session-a', 'project-a', '/work/project-a', 'Fixture session', '1.2.3');

INSERT INTO message (id, session_id, time_created, time_updated, data)
VALUES
    (
        'assistant-a',
        'session-a',
        1000,
        1000,
        '{"role":"assistant","cost":1.25,"tokens":{"input":10,"output":20,"cache":{"read":30,"write":40}},"modelID":"gpt-test","providerID":"github-copilot"}'
    ),
    (
        'user-a',
        'session-a',
        2000,
        2000,
        '{"role":"user"}'
    );
