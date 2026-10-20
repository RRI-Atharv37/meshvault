CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value BLOB NOT NULL
);

CREATE TABLE entries (
    key             TEXT PRIMARY KEY,
    path            TEXT NOT NULL,
    kind            INTEGER NOT NULL,
    size            INTEGER,
    mtime_ns        INTEGER,
    file_hash       BLOB,
    chunk_list_hash BLOB,
    vector          BLOB NOT NULL,
    lamport         INTEGER NOT NULL,
    origin          BLOB NOT NULL,
    origin_seq      INTEGER NOT NULL,
    deleted_at      INTEGER,
    suspect         INTEGER NOT NULL DEFAULT 0,
    file_id         BLOB
);

CREATE TABLE chunks (
    key    TEXT NOT NULL REFERENCES entries(key) ON DELETE CASCADE,
    idx    INTEGER NOT NULL,
    offset INTEGER NOT NULL,
    len    INTEGER NOT NULL,
    hash   BLOB NOT NULL,
    PRIMARY KEY (key, idx)
);

CREATE TABLE updates (
    origin       BLOB NOT NULL,
    seq          INTEGER NOT NULL,
    payload      BLOB NOT NULL,
    signature    BLOB NOT NULL,
    payload_hash BLOB NOT NULL,
    path_key     TEXT NOT NULL,
    PRIMARY KEY (origin, seq)
);

CREATE TABLE equivocations (
    origin    BLOB NOT NULL,
    seq       INTEGER NOT NULL,
    payload_a BLOB,
    sig_a     BLOB,
    payload_b BLOB,
    sig_b     BLOB,
    seen_at   INTEGER NOT NULL
);

CREATE TABLE trash (
    name       TEXT PRIMARY KEY,
    orig_path  TEXT NOT NULL,
    file_hash  BLOB NOT NULL,
    size       INTEGER NOT NULL,
    deleted_at INTEGER NOT NULL,
    op_id      BLOB NOT NULL
);

CREATE TABLE holds (
    batch_id      BLOB PRIMARY KEY,
    direction     INTEGER NOT NULL,
    origin        BLOB,
    path_set_hash BLOB NOT NULL,
    deletes       INTEGER NOT NULL,
    overwrites    INTEGER NOT NULL,
    created_at    INTEGER NOT NULL,
    items         BLOB NOT NULL
);

CREATE TABLE partials (
    file_hash BLOB PRIMARY KEY,
    temp_name TEXT NOT NULL,
    verified  BLOB NOT NULL
);
