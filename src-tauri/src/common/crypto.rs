//! 配对/控制通道的密码学原语。
//!
//! - 内置发现密钥：仅用于心跳防伪造（防局域网噪声/伪造广播，非强认证）。
//! - 配对：X25519 ECDH + PIN（HKDF 混入）→ 会话密钥 → AES-256-GCM。
//! - 长期密钥：node_key = HKDF(会话密钥,"node")，被控端 DPAPI 加密存储。
//! - 控制会话：每次连接做临时 ECDH，用 node_key 认证；发起方/应答方使用
//!   不同方向的密钥，避免同 key 下 nonce 重用。

use aes_gcm::aead::{Aead as AeadTrait, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

/// 两个安装包内置的同一发现密钥（防伪造广播/噪声；真正的认证靠配对）。
pub const DISCOVERY_KEY_HEX: &str =
    "339913ff1c2a3630beab01105333363ef2060ebf7356927f29e264fca6028f9a";

pub fn discovery_key() -> [u8; 32] {
    let mut out = [0u8; 32];
    hex::decode_to_slice(DISCOVERY_KEY_HEX, &mut out).expect("内置发现密钥格式错误");
    out
}

pub fn hmac_sha256_hex(key: &[u8], msg: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC key");
    mac.update(msg);
    hex::encode(mac.finalize().into_bytes())
}

pub fn hmac_sha256_verify(key: &[u8], msg: &[u8], sig_hex: &str) -> bool {
    let expect = hmac_sha256_hex(key, msg);
    if expect.len() != sig_hex.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in expect.bytes().zip(sig_hex.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    OsRng.fill_bytes(&mut v);
    v
}

pub fn random_ephemeral() -> (StaticSecret, PublicKey) {
    let sk = StaticSecret::random_from_rng(OsRng);
    let pk = PublicKey::from(&sk);
    (sk, pk)
}

pub fn ecdh(sk: &StaticSecret, peer: &PublicKey) -> [u8; 32] {
    sk.diffie_hellman(peer).to_bytes()
}

/// 由 ECDH 共享秘密派生两个方向密钥：(发起方→应答方, 应答方→发起方)
pub fn derive_session_keys(shared: &[u8; 32], salt: &[u8]) -> ([u8; 32], [u8; 32]) {
    let hk = Hkdf::<Sha256>::new(Some(salt), shared);
    let mut i2r = [0u8; 32];
    let mut r2i = [0u8; 32];
    hk.expand(b"llanllm/v1/i2r", &mut i2r).expect("HKDF expand");
    hk.expand(b"llanllm/v1/r2i", &mut r2i).expect("HKDF expand");
    (i2r, r2i)
}

/// 长期节点密钥（配对成功后双方各自保存）
pub fn derive_node_key(session: &[u8; 32]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, session);
    let mut okm = [0u8; 32];
    hk.expand(b"llanllm/v1/node-key", &mut okm).expect("HKDF expand");
    okm
}

pub struct Aead {
    key: [u8; 32],
    cipher: Aes256Gcm,
}

impl Aead {
    pub fn new(key: [u8; 32]) -> Self {
        let cipher = Aes256Gcm::new_from_slice(&key).expect("AEAD key");
        Aead { key, cipher }
    }

    /// 密封：输出 [nonce(12)][ct+tag]。nonce = 4B 方向前缀 + 8B seq BE，
    /// 密钥每连接唯一（临时 ECDH），方向分离，同 key 内 seq 不重复。
    pub fn seal(&self, seq: u64, dir_tag: u8, plaintext: &[u8], aad: &[u8]) -> Vec<u8> {
        let nonce = nonce_for(seq, dir_tag);
        let ct = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: plaintext, aad })
            .expect("AEAD encrypt");
        let mut out = Vec::with_capacity(12 + ct.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        out
    }

    pub fn open(&self, seq: u64, dir_tag: u8, data: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
        if data.len() < 12 {
            return Err("AEAD 帧过短".into());
        }
        let (nonce, ct) = data.split_at(12);
        let expect = nonce_for(seq, dir_tag);
        if nonce != expect.as_slice() {
            return Err("AEAD nonce/序列号不匹配（疑似重放）".into());
        }
        self.cipher
            .decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad })
            .map_err(|_| "AEAD 解密失败（密钥不匹配或数据被篡改）".to_string())
    }

    pub fn key_hex(&self) -> String {
        hex::encode(self.key)
    }
}

/// 一次性密封（自由函数封装，握手阶段用）
pub fn aead_seal(key: &[u8; 32], seq: u64, dir_tag: u8, plaintext: &[u8], aad: &[u8]) -> Vec<u8> {
    Aead::new(*key).seal(seq, dir_tag, plaintext, aad)
}

pub fn aead_open(
    key: &[u8; 32],
    seq: u64,
    dir_tag: u8,
    data: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, String> {
    Aead::new(*key).open(seq, dir_tag, data, aad)
}

fn nonce_for(seq: u64, dir_tag: u8) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[0] = dir_tag; // 0x01 = 发起方→应答方, 0x02 = 应答方→发起方
    n[4..].copy_from_slice(&seq.to_be_bytes());
    n
}

pub fn gen_pin6() -> String {
    format!("{:06}", OsRng.next_u32() % 1_000_000)
}

// —— DPAPI（Windows）——

/// 用 DPAPI（用户作用域）加密持久化长期密钥。
#[cfg(windows)]
pub fn dpapi_protect(plain: &[u8]) -> Result<Vec<u8>, String> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};

    unsafe {
        let mut input = CRYPT_INTEGER_BLOB {
            cbData: plain.len() as u32,
            pbData: plain.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB::default();
        CryptProtectData(&mut input, None, None, None, None, 0, &mut out)
            .map_err(|e| format!("DPAPI 加密失败: {e}"))?;
        let slice = std::slice::from_raw_parts(out.pbData, out.cbData as usize);
        let v = slice.to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData as *mut _)));
        Ok(v)
    }
}

#[cfg(windows)]
pub fn dpapi_unprotect(enc: &[u8]) -> Result<Vec<u8>, String> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    unsafe {
        let mut input = CRYPT_INTEGER_BLOB {
            cbData: enc.len() as u32,
            pbData: enc.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(&mut input, None, None, None, None, 0, &mut out)
            .map_err(|e| format!("DPAPI 解密失败: {e}"))?;
        let slice = std::slice::from_raw_parts(out.pbData, out.cbData as usize);
        let v = slice.to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData as *mut _)));
        Ok(v)
    }
}

#[cfg(not(windows))]
pub fn dpapi_protect(plain: &[u8]) -> Result<Vec<u8>, String> {
    // 非 Windows 首期不做适配：直接原样存储并加 magic（仅预留架构）
    let mut v = b"RAW1:".to_vec();
    v.extend_from_slice(plain);
    Ok(v)
}

#[cfg(not(windows))]
pub fn dpapi_unprotect(enc: &[u8]) -> Result<Vec<u8>, String> {
    enc.strip_prefix(b"RAW1:")
        .map(|s| s.to_vec())
        .ok_or_else(|| "密钥文件格式错误".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_roundtrip() {
        let sig = hmac_sha256_hex(&discovery_key(), b"hello");
        assert!(hmac_sha256_verify(&discovery_key(), b"hello", &sig));
        assert!(!hmac_sha256_verify(&discovery_key(), b"hello2", &sig));
    }

    #[test]
    fn aead_roundtrip_both_dirs() {
        let k = Aead::new([7u8; 32]);
        let msg = b"secret payload";
        let sealed = k.seal(3, 0x01, msg, b"aad");
        assert_eq!(k.open(3, 0x01, &sealed, b"aad").unwrap(), msg);
        assert!(k.open(4, 0x01, &sealed, b"aad").is_err());
        assert!(k.open(3, 0x01, &sealed, b"other").is_err());
        assert!(k.open(3, 0x02, &sealed, b"aad").is_err());
    }

    #[test]
    fn session_keys_differ_by_direction() {
        let (a, b) = derive_session_keys(&[1u8; 32], b"salt");
        assert_ne!(a, b);
        let (c, d) = derive_session_keys(&[1u8; 32], b"salt2");
        assert_ne!(a, c);
        assert_ne!(b, d);
    }

    #[test]
    fn pin_shape() {
        let p = gen_pin6();
        assert_eq!(p.len(), 6);
        assert!(p.bytes().all(|b| b.is_ascii_digit()));
    }

    #[test]
    fn ecdh_shared_matches() {
        let (s1, p1) = random_ephemeral();
        let (s2, p2) = random_ephemeral();
        assert_eq!(ecdh(&s1, &p2), ecdh(&s2, &p1));
    }
}
