//! Auth token generation.
//!
//! Mirrors `ft-server.ps1`: 24 random bytes, each mapped onto a 57-char alphabet
//! by `byte % len`. The alphabet deliberately omits look-alike characters
//! (`O`/`o`/`l`/`1`). The token is opaque on the wire (the client echoes it
//! verbatim in `AUTH <token>`), so only the server actually generates it.

/// Token alphabet: no `O`, `o`, `l`, `1` to avoid transcription mistakes.
pub const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789";

/// Number of random bytes drawn (one alphabet character each).
pub const TOKEN_BYTES: usize = 24;

/// Generate a fresh random token (24 characters).
pub fn generate() -> String {
    let mut rb = [0u8; TOKEN_BYTES];
    getrandom::getrandom(&mut rb).expect("OS RNG failed");
    rb.iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_and_alphabet() {
        let t = generate();
        assert_eq!(t.chars().count(), TOKEN_BYTES);
        for c in t.chars() {
            assert!(ALPHABET.contains(&(c as u8)), "char {c} not in alphabet");
        }
    }

    #[test]
    fn alphabet_has_no_lookalikes() {
        assert_eq!(ALPHABET.len(), 57);
        for bad in [b'O', b'o', b'l', b'1', b'0'] {
            assert!(!ALPHABET.contains(&bad), "alphabet must not contain {}", bad as char);
        }
    }
}
