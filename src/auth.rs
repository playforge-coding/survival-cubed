//! Password hashing for server-side account authentication.
//!
//! Passwords are never stored in the clear. Each account is hashed with
//! [Argon2id](https://en.wikipedia.org/wiki/Argon2) — a memory-hard function
//! designed to stay expensive to brute-force even on GPUs and custom hardware,
//! which keeps credentials safe on busy public servers, not just small ones. A
//! fresh random salt is generated per account, so identical passwords never
//! produce the same hash and precomputed-table attacks don't apply.
//!
//! The stored form is a standard [PHC string], which bundles the algorithm, its
//! cost parameters, the salt, and the digest in one self-describing record:
//!
//! ```text
//! $argon2id$v=19$m=19456,t=2,p=1$<salt_b64>$<hash_b64>
//! ```
//!
//! Because the parameters travel inside the record, cost can be raised in future
//! builds without invalidating accounts hashed under the old settings.
//!
//! [PHC string]: https://github.com/P-H-C/phc-string-format/blob/master/phc-sf-spec.md

use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};

/// Length of the random per-account salt, in bytes (Argon2's recommended size).
const SALT_LEN: usize = 16;

/// Hash `password` for storage with Argon2id (library default cost parameters,
/// which target a sensible memory/time tradeoff). Returns the encoded PHC string
/// to persist in the account record.
///
/// On the practically-impossible event of a hashing failure (e.g. the OS RNG
/// being unavailable), this logs and returns an unusable record that no password
/// will ever verify against, rather than panicking inside a connection task.
pub fn hash_password(password: &str) -> String {
    match try_hash(password) {
        Ok(encoded) => encoded,
        Err(e) => {
            log::error!("password hashing failed: {e}");
            // A record that PHC parsing rejects, so `verify_password` is always
            // false for it (the account simply can't be logged into).
            String::from("!invalid")
        }
    }
}

fn try_hash(password: &str) -> Result<String, argon2::password_hash::Error> {
    let mut salt_bytes = [0u8; SALT_LEN];
    getrandom::fill(&mut salt_bytes).map_err(|_| argon2::password_hash::Error::Crypto)?;
    let salt = SaltString::encode_b64(&salt_bytes)?;
    let hash = Argon2::default().hash_password(password.as_bytes(), &salt)?;
    Ok(hash.to_string())
}

/// Verify `password` against a previously [`hash_password`]ed record. Returns
/// `false` for any mismatch or for a record this build can't parse, never
/// erroring. Argon2's verification is constant-time with respect to the digest.
pub fn verify_password(password: &str, stored: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_correct_password() {
        let h = hash_password("hunter2");
        assert!(
            h.starts_with("$argon2id$"),
            "stored as an Argon2id PHC string"
        );
        assert!(verify_password("hunter2", &h));
    }

    #[test]
    fn rejects_wrong_password() {
        let h = hash_password("hunter2");
        assert!(!verify_password("hunter3", &h));
        assert!(!verify_password("", &h));
    }

    #[test]
    fn salts_differ_between_hashes() {
        // Same password hashed twice yields different encodings (random salt),
        // yet both verify.
        let a = hash_password("same");
        let b = hash_password("same");
        assert_ne!(a, b);
        assert!(verify_password("same", &a));
        assert!(verify_password("same", &b));
    }

    #[test]
    fn rejects_malformed_records() {
        assert!(!verify_password("x", ""));
        assert!(!verify_password("x", "garbage"));
        assert!(!verify_password("x", "!invalid"));
        assert!(!verify_password("x", "$argon2id$v=19$bogus"));
    }
}
