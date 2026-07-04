use std::io::Read;
use std::path::Path;

/// Hash any reader with BLAKE3 in 64 KiB chunks. Returns lowercase hex.
pub fn hash_reader<R: Read>(reader: &mut R) -> std::io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Hash a file on disk by streaming it.
pub fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    hash_reader(&mut f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vector_matches() {
        // BLAKE3 of the empty input is a fixed, well-known digest.
        let mut empty: &[u8] = b"";
        let got = hash_reader(&mut empty).unwrap();
        assert_eq!(
            got,
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn same_bytes_same_hash() {
        let mut a: &[u8] = b"hello world";
        let mut b: &[u8] = b"hello world";
        assert_eq!(hash_reader(&mut a).unwrap(), hash_reader(&mut b).unwrap());
    }
}
