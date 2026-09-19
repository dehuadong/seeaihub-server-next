CREATE SCHEMA IF NOT EXISTS catalog;
CREATE SCHEMA IF NOT EXISTS identity;
CREATE SCHEMA IF NOT EXISTS supply;
CREATE SCHEMA IF NOT EXISTS publication;
CREATE SCHEMA IF NOT EXISTS pricing;
CREATE SCHEMA IF NOT EXISTS generation;
CREATE SCHEMA IF NOT EXISTS ledger;
CREATE SCHEMA IF NOT EXISTS operations;

CREATE TABLE catalog.vendor_models (
    id uuid PRIMARY KEY,
    vendor_id text NOT NULL,
    native_model_id text NOT NULL,
    native_revision text NOT NULL,
    capability_schema jsonb NOT NULL,
    schema_hash text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (vendor_id, native_model_id, native_revision, schema_hash)
);

CREATE TABLE supply.channels (
    id uuid PRIMARY KEY,
    provider_kind text NOT NULL,
    base_url text NOT NULL,
    credential_env text NOT NULL,
    enabled boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (base_url !~ '/$')
);

CREATE TABLE supply.offerings (
    id uuid PRIMARY KEY,
    vendor_model_id uuid NOT NULL REFERENCES catalog.vendor_models(id),
    channel_id uuid NOT NULL REFERENCES supply.channels(id),
    adapter_key text NOT NULL,
    provider_model_id text NOT NULL,
    restrictions jsonb NOT NULL DEFAULT '{}'::jsonb,
    enabled boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE pricing.price_plans (
    id uuid PRIMARY KEY,
    offering_id uuid NOT NULL REFERENCES supply.offerings(id),
    currency text NOT NULL,
    text_input_microusd_per_million bigint NOT NULL CHECK (text_input_microusd_per_million >= 0),
    image_input_microusd_per_million bigint NOT NULL CHECK (image_input_microusd_per_million >= 0),
    text_output_microusd_per_million bigint NOT NULL CHECK (text_output_microusd_per_million >= 0),
    image_output_microusd_per_million bigint NOT NULL CHECK (image_output_microusd_per_million >= 0),
    source_url text NOT NULL,
    approved_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE publication.runtime_revisions (
    id uuid PRIMARY KEY,
    snapshot jsonb NOT NULL,
    published_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE publication.runtime_entries (
    runtime_revision_id uuid NOT NULL REFERENCES publication.runtime_revisions(id),
    vendor_model_id uuid NOT NULL REFERENCES catalog.vendor_models(id),
    offering_id uuid NOT NULL REFERENCES supply.offerings(id),
    price_plan_id uuid NOT NULL REFERENCES pricing.price_plans(id),
    native_model_id text NOT NULL,
    active boolean NOT NULL DEFAULT false,
    PRIMARY KEY (runtime_revision_id, offering_id)
);

CREATE UNIQUE INDEX one_active_runtime_entry_per_model
    ON publication.runtime_entries (native_model_id) WHERE active;

CREATE TABLE generation.assets (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL,
    role text NOT NULL,
    object_key text NOT NULL UNIQUE,
    media_type text NOT NULL,
    byte_count bigint NOT NULL CHECK (byte_count > 0),
    width integer NOT NULL CHECK (width > 0),
    height integer NOT NULL CHECK (height > 0),
    sha256 text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE ledger.accounts (
    id uuid PRIMARY KEY,
    balance_microusd bigint NOT NULL DEFAULT 0 CHECK (balance_microusd >= 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE identity.api_keys (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES ledger.accounts(id),
    label text NOT NULL,
    key_hash text NOT NULL UNIQUE,
    revoked_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE generation.jobs (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES ledger.accounts(id),
    idempotency_key text NOT NULL,
    request_hash text NOT NULL,
    state text NOT NULL,
    branch text NOT NULL,
    native_model_id text NOT NULL,
    native_parameters jsonb NOT NULL,
    asset_bindings jsonb NOT NULL,
    runtime_revision_id uuid NOT NULL REFERENCES publication.runtime_revisions(id),
    vendor_model_id uuid NOT NULL REFERENCES catalog.vendor_models(id),
    offering_id uuid NOT NULL REFERENCES supply.offerings(id),
    channel_id uuid NOT NULL REFERENCES supply.channels(id),
    price_snapshot jsonb NOT NULL,
    max_cost_microusd bigint NOT NULL CHECK (max_cost_microusd > 0),
    result_asset_ids uuid[] NOT NULL DEFAULT '{}',
    error_code text,
    error_message text,
    version bigint NOT NULL DEFAULT 0,
    lease_owner text,
    lease_expires_at timestamptz,
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (account_id, idempotency_key),
    CHECK (state IN ('accepted', 'leased', 'submitting', 'succeeded', 'failed', 'reconciliation_required', 'canceled')),
    CHECK (branch IN ('prompt_only', 'image_conditioned', 'masked'))
);

CREATE INDEX jobs_claimable
    ON generation.jobs (next_attempt_at, created_at)
    WHERE state = 'accepted';

CREATE TABLE generation.attempts (
    id uuid PRIMARY KEY,
    job_id uuid NOT NULL REFERENCES generation.jobs(id),
    state text NOT NULL,
    request_digest text NOT NULL,
    provider_trace_id text,
    provider_error_code text,
    provider_error_message text,
    response_digest text,
    metering_evidence jsonb,
    started_at timestamptz NOT NULL DEFAULT now(),
    completed_at timestamptz,
    UNIQUE (job_id),
    CHECK (state IN ('submitting', 'succeeded', 'failed', 'reconciliation_required'))
);

CREATE TABLE ledger.holds (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES ledger.accounts(id),
    job_id uuid NOT NULL REFERENCES generation.jobs(id),
    amount_microusd bigint NOT NULL CHECK (amount_microusd > 0),
    status text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (job_id),
    CHECK (status IN ('active', 'captured', 'released'))
);

CREATE TABLE ledger.entries (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES ledger.accounts(id),
    job_id uuid REFERENCES generation.jobs(id),
    kind text NOT NULL,
    amount_microusd bigint NOT NULL,
    business_key text NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (kind IN ('credit', 'hold', 'capture', 'release', 'adjustment'))
);

CREATE TABLE operations.reconciliation_cases (
    id uuid PRIMARY KEY,
    job_id uuid NOT NULL REFERENCES generation.jobs(id),
    attempt_id uuid NOT NULL REFERENCES generation.attempts(id),
    reason text NOT NULL,
    status text NOT NULL DEFAULT 'open',
    refund_note text,
    refund_business_key text UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    resolved_at timestamptz,
    UNIQUE (job_id),
    CHECK (status IN ('open', 'resolved'))
);

CREATE TABLE operations.audit_events (
    id uuid PRIMARY KEY,
    actor text NOT NULL,
    action text NOT NULL,
    subject_type text NOT NULL,
    subject_id text NOT NULL,
    payload jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
