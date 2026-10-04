-- A database written by the prototype: the baseline schema with no user_version.
CREATE TABLE workspaces (
    name TEXT PRIMARY KEY
);
CREATE TABLE repos (
    path TEXT PRIMARY KEY,
    alias TEXT,
    default_workspace TEXT NOT NULL REFERENCES workspaces(name),
    last_used TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE items (
    path TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('worktree', 'carnet')),
    repo TEXT REFERENCES repos(path),
    group_key TEXT NOT NULL DEFAULT '',
    workspace TEXT NOT NULL DEFAULT 'vrac' REFERENCES workspaces(name)
);
CREATE TABLE tabs (
    path TEXT PRIMARY KEY REFERENCES items(path),
    session TEXT NOT NULL,
    tab_id INTEGER NOT NULL,
    pane_id TEXT NOT NULL
);
CREATE TABLE cache (
    source TEXT NOT NULL,
    key TEXT NOT NULL,
    json TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    PRIMARY KEY (source, key)
);
INSERT INTO workspaces(name) VALUES ('vrac'), ('conf');
INSERT INTO repos(path, alias, default_workspace) VALUES ('/home/me/configue', NULL, 'conf');
INSERT INTO items(path, kind, repo, group_key, workspace)
    VALUES ('/home/me/configue', 'worktree', '/home/me/configue', '', 'conf'),
           ('/home/me/Data/2026-09-30-notes', 'carnet', NULL, '', 'vrac');
INSERT INTO tabs(path, session, tab_id, pane_id) VALUES ('/home/me/configue', 'conf', 2, '5');
INSERT INTO cache(source, key, json, fetched_at) VALUES ('gh', 'reviews', '[]', '2026-09-30T00:00:00+00:00');
