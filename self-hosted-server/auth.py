"""OAuth 2.0 Device Authorization Grant backed by Supabase Auth (Google provider).

Flow:
  1. CLI POSTs /worker/oauth/device/code → we mint a device_code + user_code.
  2. User opens /device?user_code=... in a browser. A minimal page immediately
     triggers supabase.auth.signInWithOAuth({provider:"google"}), so the only UI
     the user actually sees is Google's consent screen.
  3. Supabase redirects to /device/callback. A tiny script picks up the session
     and POSTs its JWT to /device/approve, which binds it to the user_code.
  4. The CLI's polling hits /worker/oauth/token and receives the Supabase JWT.
"""
import base64
import hashlib
import json
import os
import secrets
from datetime import datetime, timedelta, timezone
from pathlib import Path

from fastapi import APIRouter, HTTPException, Request
from fastapi.responses import JSONResponse
from fastapi.templating import Jinja2Templates
from pydantic import BaseModel

from db import supabase_admin, get_anon_client, get_authenticated_client


def _hash_device_code(code: str) -> str:
    return hashlib.sha256(code.encode("utf-8")).hexdigest()


def _jwt_exp(token: str) -> int:
    """Read the `exp` claim from a JWT. Caller must validate the signature separately."""
    try:
        _, payload_b64, _ = token.split(".")
    except ValueError:
        raise HTTPException(status_code=401, detail="Malformed JWT")
    padding = "=" * (-len(payload_b64) % 4)
    try:
        payload = json.loads(base64.urlsafe_b64decode(payload_b64 + padding))
        return int(payload["exp"])
    except (ValueError, KeyError, json.JSONDecodeError):
        raise HTTPException(status_code=401, detail="JWT missing or invalid `exp` claim")

router = APIRouter()
templates = Jinja2Templates(directory=str(Path(__file__).parent / "templates"))

DEVICE_CODE_TTL_SECONDS = 900  # 15 min
POLL_INTERVAL_SECONDS = 5
REFRESH_TOKEN_TTL_SECONDS = 30 * 24 * 3600  # Supabase doesn't expose this; use a sane default

SUPABASE_URL = os.environ["SUPABASE_URL"]
SUPABASE_ANON_KEY = os.environ["SUPABASE_ANON_KEY"]
BASE_URL = os.environ.get("BASE_URL", "http://localhost:8000")

USER_CODE_ALPHABET = "BCDFGHJKLMNPQRSTVWXZ"  # no vowels/ambiguous chars


def _oauth_error(code: str, description: str | None = None, status: int = 400) -> JSONResponse:
    body: dict = {"error": code}
    if description:
        body["error_description"] = description
    return JSONResponse(status_code=status, content=body)


# ---------------- CLI-facing OAuth endpoints ----------------


@router.post("/worker/oauth/device/code")
async def device_code():
    device_code_val = secrets.token_urlsafe(32)
    user_code = "".join(secrets.choice(USER_CODE_ALPHABET) for _ in range(8))
    expires_at = datetime.now(timezone.utc) + timedelta(seconds=DEVICE_CODE_TTL_SECONDS)

    try:
        supabase_admin.table("device_codes").insert({
            "device_code_hash": _hash_device_code(device_code_val),
            "user_code": user_code,
            "status": "pending",
            "expires_at": expires_at.isoformat(),
        }).execute()
    except Exception as e:
        return _oauth_error("server_error", f"Failed to create device code: {e}", status=500)

    verification_uri = f"{BASE_URL}/device"
    return {
        "device_code": device_code_val,
        "user_code": user_code,
        "verification_uri": verification_uri,
        "verification_uri_complete": f"{verification_uri}?user_code={user_code}",
        "expires_in": DEVICE_CODE_TTL_SECONDS,
        "interval": POLL_INTERVAL_SECONDS,
    }


@router.post("/worker/oauth/token")
async def oauth_token(request: Request):
    try:
        body = await request.json()
    except Exception:
        return _oauth_error("invalid_request", "Invalid JSON")

    grant_type = body.get("grant_type")

    if grant_type == "urn:ietf:params:oauth:grant-type:device_code":
        return _exchange_device_code(body.get("device_code"))

    if grant_type == "refresh_token":
        return _refresh(body.get("refresh_token"))

    return _oauth_error("unsupported_grant_type", f"Unsupported grant_type: {grant_type}")


def _exchange_device_code(device_code_val: str | None):
    if not device_code_val:
        return _oauth_error("invalid_request", "Missing device_code")

    device_code_hash = _hash_device_code(device_code_val)
    try:
        resp = (
            supabase_admin.table("device_codes")
            .select("*")
            .eq("device_code_hash", device_code_hash)
            .limit(1)
            .execute()
        )
    except Exception as e:
        return _oauth_error("server_error", f"Database error: {e}", status=500)

    if not resp.data:
        return _oauth_error("invalid_grant", "Unknown device_code")
    row = resp.data[0]

    expires_at = datetime.fromisoformat(row["expires_at"])
    if datetime.now(timezone.utc) > expires_at:
        try:
            supabase_admin.table("device_codes").delete().eq("device_code_hash", device_code_hash).execute()
        except Exception:
            pass  # best-effort cleanup
        return _oauth_error("expired_token", "Device code expired")

    status = row["status"]
    if status == "pending":
        return _oauth_error("authorization_pending", "User has not yet authorized")
    if status == "denied":
        return _oauth_error("access_denied", "User denied the request")
    if status != "approved":
        return _oauth_error("invalid_grant", f"Invalid status: {status}")

    access_expires_at = datetime.fromisoformat(row["access_token_expires_at"])
    refresh_expires_at = datetime.fromisoformat(row["refresh_token_expires_at"])
    now = datetime.now(timezone.utc)

    # One-time use: delete row after handing out the tokens.
    try:
        supabase_admin.table("device_codes").delete().eq("device_code_hash", device_code_hash).execute()
    except Exception:
        pass  # best-effort cleanup; tokens are still valid

    return {
        "access_token": row["supabase_access_token"],
        "token_type": "Bearer",
        "expires_in": max(int((access_expires_at - now).total_seconds()), 0),
        "refresh_token": row["supabase_refresh_token"],
        "refresh_expires_in": max(int((refresh_expires_at - now).total_seconds()), 0),
    }


def _refresh(refresh_token: str | None):
    if not refresh_token:
        return _oauth_error("invalid_request", "Missing refresh_token")
    # refresh_session mutates the calling client's stored session, so use a
    # throwaway client to keep the long-lived globals clean.
    try:
        result = get_anon_client().auth.refresh_session(refresh_token)
    except Exception as e:
        return _oauth_error("invalid_grant", str(e))

    session = getattr(result, "session", None)
    if not session:
        return _oauth_error("invalid_grant", "Refresh failed")

    return {
        "access_token": session.access_token,
        "token_type": "Bearer",
        "expires_in": session.expires_in,
        "refresh_token": session.refresh_token,
        "refresh_expires_in": REFRESH_TOKEN_TTL_SECONDS,
    }


# ---------------- Browser-facing web flow ----------------


@router.get("/device")
async def device_page(request: Request, user_code: str | None = None):
    return templates.TemplateResponse(
        request,
        "device.html",
        {
            "user_code": user_code or "",
            "supabase_url": SUPABASE_URL,
            "supabase_anon_key": SUPABASE_ANON_KEY,
        },
    )


@router.get("/device/callback")
async def device_callback(request: Request, user_code: str):
    return templates.TemplateResponse(
        request,
        "device_callback.html",
        {
            "user_code": user_code,
            "supabase_url": SUPABASE_URL,
            "supabase_anon_key": SUPABASE_ANON_KEY,
        },
    )


class ApproveRequest(BaseModel):
    user_code: str
    access_token: str
    refresh_token: str


@router.post("/device/approve")
async def device_approve(req: ApproveRequest):
    try:
        user_resp = supabase_admin.auth.get_user(req.access_token)
    except Exception as e:
        raise HTTPException(status_code=401, detail=f"Invalid token: {e}")

    user = getattr(user_resp, "user", None)
    if not user:
        raise HTTPException(status_code=401, detail="Invalid token")

    access_expires_at = datetime.fromtimestamp(_jwt_exp(req.access_token), tz=timezone.utc)
    now = datetime.now(timezone.utc)
    refresh_expires_at = now + timedelta(seconds=REFRESH_TOKEN_TTL_SECONDS)

    try:
        result = (
            supabase_admin.table("device_codes")
            .update({
                "status": "approved",
                "supabase_access_token": req.access_token,
                "supabase_refresh_token": req.refresh_token,
                "access_token_expires_at": access_expires_at.isoformat(),
                "refresh_token_expires_at": refresh_expires_at.isoformat(),
                "user_id": user.id,
            })
            .eq("user_code", req.user_code)
            .eq("status", "pending")
            .gt("expires_at", now.isoformat())
            .execute()
        )
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"Database error: {e}")

    if not result.data:
        raise HTTPException(status_code=400, detail="Invalid or expired user_code")

    return {"ok": True}


# ---------------- Auth dependency for protected routes ----------------


def _extract_token(request: Request) -> str:
    header = request.headers.get("Authorization", "")
    if not header.startswith("Bearer "):
        raise HTTPException(status_code=401, detail="Missing bearer token")
    return header[7:]


async def require_auth(request: Request):
    """Validate the bearer token and return the user.

    Creates a per-request Supabase client with the user's JWT for RLS
    and stores it on request.state.supabase so endpoint handlers can use
    it without mutating the global client.
    """
    token = _extract_token(request)
    try:
        user_resp = supabase_admin.auth.get_user(token)
    except Exception as e:
        raise HTTPException(status_code=401, detail=f"Invalid token: {e}")
    user = getattr(user_resp, "user", None)
    if not user:
        raise HTTPException(status_code=401, detail="Invalid token")
    request.state.supabase = get_authenticated_client(token)
    return user
