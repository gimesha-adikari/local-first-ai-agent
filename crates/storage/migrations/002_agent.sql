CREATE TABLE agent_tasks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    state TEXT NOT NULL DEFAULT 'created' CHECK (state IN (
        'created', 'planning', 'validating', 'awaiting_authorization', 'authorized',
        'executing', 'observing', 'completed', 'denied', 'cancelled', 'failed',
        'timed_out', 'interrupted'
    )),
    started INTEGER NOT NULL DEFAULT 0 CHECK (started IN (0, 1)),
    finalized INTEGER NOT NULL DEFAULT 0 CHECK (finalized IN (0, 1)),
    transition_sequence INTEGER NOT NULL DEFAULT 0 CHECK (transition_sequence >= 0),
    event_sequence INTEGER NOT NULL DEFAULT 0 CHECK (event_sequence >= 0),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at),
    usage TEXT,
    error TEXT,
    CHECK (finalized = 0 OR state IN ('completed','denied','cancelled','failed','timed_out','interrupted'))
) STRICT;

CREATE UNIQUE INDEX agent_single_active_task ON agent_tasks(finalized) WHERE finalized = 0;

CREATE TABLE agent_task_events (
    task_id INTEGER NOT NULL REFERENCES agent_tasks(id),
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    event TEXT NOT NULL CHECK (length(event) <= 4096),
    PRIMARY KEY (task_id, sequence)
) STRICT;

CREATE TABLE agent_executions (
    operation_id TEXT PRIMARY KEY CHECK (length(operation_id) BETWEEN 1 AND 64),
    task_id INTEGER NOT NULL REFERENCES agent_tasks(id),
    tool TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending','not_dispatched','succeeded','failed','uncertain')),
    error TEXT,
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at)
) STRICT;

CREATE INDEX agent_executions_by_task ON agent_executions(task_id, created_at, operation_id);

CREATE TABLE permission_audit (
    namespace TEXT NOT NULL CHECK (length(namespace) BETWEEN 1 AND 32),
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    task_id INTEGER REFERENCES agent_tasks(id),
    operation_id TEXT,
    tool TEXT,
    action TEXT NOT NULL,
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    record TEXT NOT NULL CHECK (length(record) <= 4096),
    PRIMARY KEY (namespace, sequence)
) STRICT;

CREATE INDEX permission_audit_by_task ON permission_audit(task_id, namespace, sequence);
CREATE INDEX permission_audit_by_operation ON permission_audit(operation_id, task_id, tool, action);

-- An omitted count belongs to a nonempty batch identified by its first
-- sequence. Store it transactionally alongside the records, idempotently.
CREATE TABLE permission_audit_batches (
    namespace TEXT NOT NULL,
    first_sequence INTEGER NOT NULL,
    last_sequence INTEGER NOT NULL CHECK (last_sequence >= first_sequence),
    omitted INTEGER NOT NULL CHECK (omitted >= 0),
    PRIMARY KEY (namespace, first_sequence),
    FOREIGN KEY (namespace, first_sequence) REFERENCES permission_audit(namespace, sequence),
    FOREIGN KEY (namespace, last_sequence) REFERENCES permission_audit(namespace, sequence)
) STRICT;
