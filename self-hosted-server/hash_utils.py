import hashlib
import jcs


def verify_hash(content, claimed_hash: str) -> bool:
    """Verify that claimed_hash matches SHA-256 of RFC 8785 canonicalized content."""
    canonical_bytes = jcs.canonicalize(content)
    computed = hashlib.sha256(canonical_bytes).hexdigest()
    return computed == claimed_hash
