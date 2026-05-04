import os
from supabase import create_client, Client
from dotenv import load_dotenv

load_dotenv()

SUPABASE_URL = os.environ["SUPABASE_URL"]
SUPABASE_ANON_KEY = os.environ["SUPABASE_ANON_KEY"]

# Global client — used ONLY for unauthenticated operations (device flow).
# NEVER call .postgrest.auth() on this client; use get_authenticated_client() instead.
supabase: Client = create_client(SUPABASE_URL, SUPABASE_ANON_KEY)


def get_authenticated_client(token: str) -> Client:
    """Create a per-request Supabase client with the user's JWT set for RLS.

    This avoids mutating the global client's auth state, which caused
    concurrent requests to clobber each other and anon operations to
    fail with stale/expired JWTs.
    """
    client = create_client(SUPABASE_URL, SUPABASE_ANON_KEY)
    client.postgrest.auth(token)
    return client
