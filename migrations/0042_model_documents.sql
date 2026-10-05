-- 模型使用文档：素材版本与已发布正文（Spec 0008 §3–§4；设计见
-- .agents/notes/implemented/platform/2026-10-05-model-usage-documentation.md「发布与持久化」）。
--
-- 素材版本关联 Vendor Model，不影响合同唯一键与不可变规则：同一内容重复导入复用一行，
-- 内容变化追加一行，发布取该厂商模型最近导入的那一行。已发布正文单独落表并关联 Runtime
-- Revision，发布即冻结——素材文件或最新指针之后怎么变都不改已发布正文。
CREATE TABLE publication.model_document_materials (
    id uuid PRIMARY KEY,
    vendor_model_id uuid NOT NULL REFERENCES catalog.vendor_models(id),
    content_hash text NOT NULL,
    material jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (vendor_model_id, content_hash)
);

-- 取“最近导入的那一行”的证据：created_at 相同（同批导入）时用主键定序。
CREATE INDEX model_document_materials_latest
    ON publication.model_document_materials (vendor_model_id, created_at DESC, id DESC);

CREATE TABLE publication.model_documents (
    id uuid PRIMARY KEY,
    runtime_revision_id uuid NOT NULL UNIQUE
        REFERENCES publication.runtime_revisions(id),
    gateway_model text NOT NULL,
    vendor_model_id uuid NOT NULL REFERENCES catalog.vendor_models(id),
    body text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    -- 正文大小上限（设计 §持久化与发布：256 KiB，超限拒绝发布、不截断）。
    CONSTRAINT model_documents_body_bounded CHECK (octet_length(body) <= 262144)
);

-- 历史读取按（平台名, 文档标识）定位；当前读取与目录同一条判据。
CREATE INDEX model_documents_by_name
    ON publication.model_documents (gateway_model, id);
