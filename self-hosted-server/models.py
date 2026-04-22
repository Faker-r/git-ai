from pydantic import BaseModel
from typing import Any


class ExcludeNoneModel(BaseModel):
    """Base that omits None fields from JSON, matching Rust's skip_serializing_if."""
    def model_dump(self, **kwargs):
        kwargs.setdefault("exclude_none", True)
        return super().model_dump(**kwargs)


# --- CAS ---
# Mirrors: src/api/types.rs:84-113

class CasObject(BaseModel):
    content: Any  # serde_json::Value — any valid JSON
    hash: str  # SHA-256 hex of RFC 8785 canonicalized content
    metadata: dict[str, str] = {}  # HashMap<String, String>, omitted when empty


class CasUploadRequest(BaseModel):
    objects: list[CasObject]


class CasUploadResult(ExcludeNoneModel):
    hash: str
    status: str  # "ok" or "error"
    error: str | None = None


class CasUploadResponse(BaseModel):
    results: list[CasUploadResult]
    success_count: int
    failure_count: int


# CAS read uses a separate result type (src/api/types.rs:122-137)
class CasReadResult(ExcludeNoneModel):
    hash: str
    status: str  # "ok" or "not_found"
    content: Any | None = None
    error: str | None = None


class CasReadResponse(BaseModel):
    results: list[CasReadResult]
    success_count: int
    failure_count: int


# --- Metrics ---
# Mirrors: src/metrics/types.rs
# Wire format uses short keys: t, e, v, a at event level; v at batch level

class MetricEvent(BaseModel):
    t: int  # u32 Unix timestamp (seconds)
    e: int  # u16 event type ID (1=Committed, 2=AgentUsage, 3=InstallHooks, 4=Checkpoint)
    v: dict[str, Any] = {}  # SparseArray: HashMap<String, serde_json::Value>
    a: dict[str, Any] = {}  # SparseArray: common attributes


class MetricsBatch(BaseModel):
    v: int  # u8 API version, must be 1
    events: list[MetricEvent]


class MetricsError(BaseModel):
    index: int
    error: str


class MetricsUploadResponse(BaseModel):
    errors: list[MetricsError]


# --- Bundles ---

class CreateBundleRequest(BaseModel):
    title: str
    data: dict[str, Any]


class CreateBundleResponse(BaseModel):
    success: bool
    id: str
    url: str
