import os
from supabase import create_client, Client
from dotenv import load_dotenv

load_dotenv()

SUPABASE_URL = os.environ["SUPABASE_URL"]
SUPABASE_ANON_KEY = os.environ["SUPABASE_ANON_KEY"]

# Global client — initialized with anon key for unauthenticated operations (device flow).
# For authenticated operations, use postgrest.auth(token) to set the user's JWT
# so RLS evaluates against their identity.
supabase: Client = create_client(SUPABASE_URL, SUPABASE_ANON_KEY)
