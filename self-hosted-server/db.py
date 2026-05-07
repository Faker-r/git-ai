import os
from supabase import create_client, Client
from dotenv import load_dotenv

load_dotenv()

SUPABASE_URL = os.environ["SUPABASE_URL"]
SUPABASE_ANON_KEY = os.environ["SUPABASE_ANON_KEY"]
SUPABASE_SERVICE_KEY = os.environ["SUPABASE_KEY"]

# Service-role client for server-owned tables (device_codes), token validation,
# and the health probe. Bypasses RLS — never let user input drive its queries.
supabase_admin: Client = create_client(SUPABASE_URL, SUPABASE_SERVICE_KEY)


def get_authenticated_client(token: str) -> Client:
    """Per-request client carrying the user's JWT so PostgREST applies RLS."""
    client = create_client(SUPABASE_URL, SUPABASE_ANON_KEY)
    client.postgrest.auth(token)
    return client


def get_anon_client() -> Client:
    """Throwaway anon client for GoTrue methods that mutate session state
    (auth.refresh_session, auth.sign_in_*). Discard after the call."""
    return create_client(SUPABASE_URL, SUPABASE_ANON_KEY)
