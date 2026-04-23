-- Run this in the Supabase SQL editor before starting the server.
-- Safe to re-run: every statement uses IF NOT EXISTS.

-- Content-addressable storage for git-ai objects.
create table if not exists cas_objects (
    hash text primary key not null,
    content jsonb not null,
    metadata jsonb not null default '{}',
    created_at timestamptz not null default now(),
    uploaded_by text,
    user_id uuid
);
-- For existing deployments (must run before indexes on this column):
alter table cas_objects add column if not exists user_id uuid;
create index if not exists idx_cas_created_at on cas_objects(created_at);
create index if not exists idx_cas_user_id on cas_objects(user_id);

-- Metrics events uploaded by the CLI.
create table if not exists metrics_events (
    id bigserial primary key,
    event_json jsonb not null,
    event_id smallint not null,
    timestamp timestamptz not null,
    repo_url text,
    author text,
    tool text,
    model text,
    commit_sha text,
    branch text,
    git_ai_version text,
    user_id uuid,
    received_at timestamptz not null default now()
);
-- For existing deployments (must run before indexes on this column):
alter table metrics_events add column if not exists user_id uuid;
create index if not exists idx_metrics_timestamp on metrics_events(timestamp);
create index if not exists idx_metrics_event_id on metrics_events(event_id);
create index if not exists idx_metrics_repo_url on metrics_events(repo_url);
create index if not exists idx_metrics_author on metrics_events(author);
create index if not exists idx_metrics_tool_model on metrics_events(tool, model);
create index if not exists idx_metrics_user_id on metrics_events(user_id);

-- Pending/approved device codes for the OAuth 2.0 device flow.
create table if not exists device_codes (
    device_code text primary key,
    user_code text unique not null,
    status text not null default 'pending', -- pending | approved | denied
    expires_at timestamptz not null,
    supabase_access_token text,
    supabase_refresh_token text,
    access_token_expires_at timestamptz,
    refresh_token_expires_at timestamptz,
    user_id uuid,
    created_at timestamptz not null default now()
);
create index if not exists idx_device_codes_user_code on device_codes(user_code);
