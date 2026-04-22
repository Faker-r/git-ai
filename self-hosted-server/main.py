import os
from datetime import datetime, timezone

from fastapi import FastAPI, Query, Request
from fastapi.responses import JSONResponse
from dotenv import load_dotenv

from db import supabase
from models import (
    CasUploadRequest,
    CasUploadResponse,
    CasUploadResult,
    CasReadResponse,
    CasReadResult,
    MetricsBatch,
    MetricsUploadResponse,
    MetricsError,
)
from hash_utils import verify_hash

load_dotenv()

BASE_URL = os.environ.get("BASE_URL", "http://localhost:8000")

app = FastAPI(title="git-ai self-hosted server", version="1.0.0")


# --- Health Check ---


@app.get("/health")
async def health():
    return {"status": "ok", "version": "1.0.0"}


# --- CAS Upload ---


@app.post("/worker/cas/upload", response_model=CasUploadResponse, response_model_exclude_none=True)
async def cas_upload(req: CasUploadRequest, request: Request):
    results = []
    success_count = 0
    failure_count = 0

    for obj in req.objects:
        if not verify_hash(obj.content, obj.hash):
            results.append(CasUploadResult(hash=obj.hash, status="error", error="Hash mismatch"))
            failure_count += 1
            continue

        try:
            supabase.table("cas_objects").upsert(
                {
                    "hash": obj.hash,
                    "content": obj.content,
                    "metadata": obj.metadata,
                    "uploaded_by": request.headers.get("X-Author-Identity"),
                },
                on_conflict="hash",
                ignore_duplicates=True,
            ).execute()
            results.append(CasUploadResult(hash=obj.hash, status="ok"))
            success_count += 1
        except Exception as e:
            results.append(CasUploadResult(hash=obj.hash, status="error", error=str(e)))
            failure_count += 1

    return CasUploadResponse(results=results, success_count=success_count, failure_count=failure_count)


# --- CAS Read ---
# Client sends GET /worker/cas/?hashes=h1,h2,... (trailing slash matters)


@app.get("/worker/cas/", response_model=CasReadResponse, response_model_exclude_none=True)
async def cas_read(hashes: str = Query(...)):
    hash_list = [h.strip() for h in hashes.split(",") if h.strip()]

    if not hash_list or len(hash_list) > 100:
        return JSONResponse(status_code=400, content={"error": "Provide 1-100 comma-separated hashes"})

    try:
        resp = supabase.table("cas_objects").select("hash, content").in_("hash", hash_list).execute()
        found = {row["hash"]: row["content"] for row in resp.data}
    except Exception as e:
        return JSONResponse(status_code=500, content={"error": str(e)})

    results = []
    success_count = 0
    failure_count = 0

    for h in hash_list:
        if h in found:
            results.append(CasReadResult(hash=h, status="ok", content=found[h]))
            success_count += 1
        else:
            results.append(CasReadResult(hash=h, status="not_found"))
            failure_count += 1

    return CasReadResponse(results=results, success_count=success_count, failure_count=failure_count)


# --- Metrics Upload ---


@app.post("/worker/metrics/upload", response_model=MetricsUploadResponse)
async def metrics_upload(req: MetricsBatch, request: Request):
    errors = []

    if req.v != 1:
        return JSONResponse(status_code=400, content={"error": f"Unsupported API version: {req.v}"})

    if len(req.events) > 250:
        return JSONResponse(status_code=400, content={"error": "Max 250 events per batch"})

    rows = []
    for i, event in enumerate(req.events):
        if event.e not in (1, 2, 3, 4):
            errors.append(MetricsError(index=i, error=f"Invalid event_id: {event.e}"))
            continue

        attrs = event.a
        rows.append({
            "event_json": {"t": event.t, "e": event.e, "v": event.v, "a": event.a},
            "event_id": event.e,
            "timestamp": datetime.fromtimestamp(event.t, tz=timezone.utc).isoformat(),
            "repo_url": attrs.get("1"),
            "author": attrs.get("2"),
            "tool": attrs.get("20"),
            "model": attrs.get("21"),
            "commit_sha": attrs.get("3"),
            "branch": attrs.get("5"),
            "git_ai_version": attrs.get("0"),
        })

    if rows:
        try:
            supabase.table("metrics_events").insert(rows).execute()
        except Exception as e:
            # Batch failed — try one-by-one to isolate which events failed
            for j, row in enumerate(rows):
                try:
                    supabase.table("metrics_events").insert(row).execute()
                except Exception as inner_e:
                    errors.append(MetricsError(index=j, error=str(inner_e)))

    return MetricsUploadResponse(errors=errors)
