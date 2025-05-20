-- Add migration script here
create table if not exists query_history (
    id text primary key,
    query_id text not null,
    account text,
    status text not null,
    contents text not null,

    records_total integer,
    records_matched real,
    records_scanned real,
    bytes_scanned real,

    created_at timestamp not null,
    modified_at timestamp not null,
    deleted_at timestamp,

    unique(id, query_id)
);

create index if not exists idx_history_query_id on query_history(query_id);
