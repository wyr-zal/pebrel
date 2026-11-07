//! Browser-compatible sealed tunnel used by the DSH Mobile Suite.
//!
//! The browser owns the AES/HMAC implementation in `crypto.js`; this module
//! implements the companion half without changing the wire format. Relay
//! frames are bounded JSON strings and never contain plaintext task data.

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac as _};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

type HmacSha256 = Hmac<Sha256>;

const KEY_BYTES: usize = 32;
const RANDOM_BYTES: usize = 32;
const TAG_BYTES: usize = 16;

#[cfg(test)]
mod interop_tests;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Envelope {
    pub(crate) v: u8,
    #[serde(rename = "type")]
    pub(crate) kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) channel: Option<String>,
    pub(crate) id: String,
    pub(crate) ts: u64,
    pub(crate) payload: Value,
}

impl Envelope {
    pub(crate) fn new(kind: impl Into<String>, payload: Value, channel: Option<String>) -> Self {
        Self { v: 1, kind: kind.into(), channel, id: random_id(), ts: super::now_millis(), payload }
    }

    pub(crate) fn encode(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| error.to_string())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SealedPayload {
    pub(crate) seq: String,
    pub(crate) ciphertext_b64: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ClientHello {
    pub(crate) access_session_id: String,
    pub(crate) client_random_b64: String,
    pub(crate) client_proof_b64: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerHello {
    access_session_id: String,
    server_random_b64: String,
    server_proof_b64: String,
}

struct Material {
    c2d_key: [u8; KEY_BYTES],
    d2c_key: [u8; KEY_BYTES],
    c2d_nonce: [u8; 4],
    d2c_nonce: [u8; 4],
}

pub(crate) struct SecureCipher {
    access_session_id: String,
    key: [u8; KEY_BYTES],
    nonce_prefix: [u8; 4],
    send_direction: &'static str,
    receive_key: [u8; KEY_BYTES],
    receive_nonce_prefix: [u8; 4],
    receive_direction: &'static str,
    send_sequence: u64,
    receive_sequence: u64,
}

pub(crate) struct AcceptedHello {
    pub(crate) payload: Value,
    pub(crate) cipher: SecureCipher,
}

fn random_id() -> String {
    let mut bytes = [0_u8; 16];
    let _ = getrandom::fill(&mut bytes);
    format!("evt_{}", URL_SAFE_NO_PAD.encode(bytes))
}

pub(crate) fn random_key() -> Result<String, String> {
    let mut key = [0_u8; KEY_BYTES];
    getrandom::fill(&mut key).map_err(|error| error.to_string())?;
    Ok(URL_SAFE_NO_PAD.encode(key))
}

fn random_bytes() -> Result<[u8; RANDOM_BYTES], String> {
    let mut value = [0_u8; RANDOM_BYTES];
    getrandom::fill(&mut value).map_err(|error| error.to_string())?;
    Ok(value)
}

fn canonical(parts: &[Value]) -> Vec<u8> {
    serde_json::to_vec(parts).unwrap_or_default()
}

fn decode_key(value: &str) -> Result<[u8; KEY_BYTES], String> {
    decode_base64_url(value, Some(KEY_BYTES))?
        .try_into()
        .map_err(|_| "invalid master key".to_owned())
}

fn decode_base64_url(value: &str, length: Option<usize>) -> Result<Vec<u8>, String> {
    if !value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')) {
        return Err("invalid base64url".to_owned());
    }
    let decoded = URL_SAFE_NO_PAD.decode(value).map_err(|_| "invalid base64url".to_owned())?;
    if URL_SAFE_NO_PAD.encode(&decoded) != value {
        return Err("non-canonical base64url".to_owned());
    }
    if length.is_some_and(|expected| decoded.len() != expected) {
        return Err("invalid base64url length".to_owned());
    }
    Ok(decoded)
}

fn hmac(key: &[u8], value: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts arbitrary key lengths");
    mac.update(value);
    mac.finalize().into_bytes().into()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

fn proof(master_key: &str, label: &str, parts: &[Value]) -> Result<String, String> {
    let key = decode_key(master_key)?;
    let mut values = vec![Value::String(label.to_owned()), json!(1)];
    values.extend_from_slice(parts);
    Ok(URL_SAFE_NO_PAD.encode(hmac(&key, &canonical(&values))))
}

fn hkdf_expand(prk: &[u8], info: &[u8], length: usize) -> Vec<u8> {
    let mut output = Vec::with_capacity(length);
    let mut previous = Vec::new();
    let mut counter = 1_u8;
    while output.len() < length {
        let mut input = previous;
        input.extend_from_slice(info);
        input.push(counter);
        previous = hmac(prk, &input).to_vec();
        output.extend_from_slice(&previous);
        counter = counter.wrapping_add(1);
    }
    output.truncate(length);
    output
}

fn derive_material(
    master_key: &str,
    access_session_id: &str,
    client_random_b64: &str,
    server_random_b64: &str,
) -> Result<Material, String> {
    let key = decode_key(master_key)?;
    decode_base64_url(client_random_b64, Some(RANDOM_BYTES))?;
    decode_base64_url(server_random_b64, Some(RANDOM_BYTES))?;
    let salt = Sha256::digest(canonical(&[
        json!("dsh-e2ee-salt"),
        json!(1),
        json!(access_session_id),
        json!(client_random_b64),
        json!(server_random_b64),
    ]));
    let prk = hmac(&salt, &key);
    let expand32 = |info: &str| -> [u8; KEY_BYTES] {
        hkdf_expand(&prk, info.as_bytes(), KEY_BYTES)
            .try_into()
            .expect("HKDF output has the requested length")
    };
    let expand4 = |info: &str| -> [u8; 4] {
        hkdf_expand(&prk, info.as_bytes(), 4)
            .try_into()
            .expect("HKDF output has the requested length")
    };
    Ok(Material {
        c2d_key: expand32("dsh-e2ee-v1:c2d:key"),
        d2c_key: expand32("dsh-e2ee-v1:d2c:key"),
        c2d_nonce: expand4("dsh-e2ee-v1:c2d:nonce"),
        d2c_nonce: expand4("dsh-e2ee-v1:d2c:nonce"),
    })
}

fn nonce(prefix: [u8; 4], sequence: u64) -> [u8; 12] {
    let mut nonce = [0_u8; 12];
    nonce[..4].copy_from_slice(&prefix);
    nonce[4..].copy_from_slice(&sequence.to_be_bytes());
    nonce
}

fn aad(access_session_id: &str, direction: &str, sequence: u64) -> Vec<u8> {
    canonical(&[
        json!("dsh-e2ee"),
        json!(1),
        json!(access_session_id),
        json!(direction),
        json!(sequence.to_string()),
    ])
}

impl SecureCipher {
    fn server(material: Material, access_session_id: String) -> Self {
        Self {
            access_session_id,
            key: material.d2c_key,
            nonce_prefix: material.d2c_nonce,
            send_direction: "d2c",
            receive_key: material.c2d_key,
            receive_nonce_prefix: material.c2d_nonce,
            receive_direction: "c2d",
            send_sequence: 0,
            receive_sequence: 0,
        }
    }

    pub(crate) fn seal(&mut self, value: &Value) -> Result<SealedPayload, String> {
        let sequence = self.send_sequence;
        let cipher = Aes256Gcm::new((&self.key).into());
        let nonce = nonce(self.nonce_prefix, sequence);
        let plaintext = serde_json::to_vec(value).map_err(|error| error.to_string())?;
        let ciphertext = cipher
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: &plaintext,
                    aad: &aad(&self.access_session_id, self.send_direction, sequence),
                },
            )
            .map_err(|_| "seal failed".to_owned())?;
        self.send_sequence =
            self.send_sequence.checked_add(1).ok_or_else(|| "sequence exhausted".to_owned())?;
        Ok(SealedPayload {
            seq: sequence.to_string(),
            ciphertext_b64: URL_SAFE_NO_PAD.encode(ciphertext),
        })
    }

    pub(crate) fn open(&mut self, sealed: &SealedPayload) -> Result<Value, String> {
        if sealed.seq != "0"
            && (sealed.seq.starts_with('0')
                || sealed.seq.is_empty()
                || !sealed.seq.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return Err("invalid sequence".to_owned());
        }
        let sequence: u64 = sealed.seq.parse().map_err(|_| "invalid sequence".to_owned())?;
        if sequence != self.receive_sequence {
            return Err("unexpected sequence".to_owned());
        }
        let ciphertext = decode_base64_url(&sealed.ciphertext_b64, None)?;
        if ciphertext.len() < TAG_BYTES {
            return Err("truncated ciphertext".to_owned());
        }
        let cipher = Aes256Gcm::new((&self.receive_key).into());
        let nonce = nonce(self.receive_nonce_prefix, sequence);
        let plaintext = cipher
            .decrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &aad(&self.access_session_id, self.receive_direction, sequence),
                },
            )
            .map_err(|_| "ciphertext authentication failed".to_owned())?;
        let value =
            serde_json::from_slice(&plaintext).map_err(|_| "invalid sealed JSON".to_owned())?;
        self.receive_sequence =
            self.receive_sequence.checked_add(1).ok_or_else(|| "sequence exhausted".to_owned())?;
        Ok(value)
    }
}

pub(crate) fn accept_client_hello(
    master_key: &str,
    hello: ClientHello,
) -> Result<AcceptedHello, String> {
    let server_random_b64 = URL_SAFE_NO_PAD.encode(random_bytes()?);
    accept_client_hello_with_server_random(master_key, hello, &server_random_b64)
}

fn accept_client_hello_with_server_random(
    master_key: &str,
    hello: ClientHello,
    server_random_b64: &str,
) -> Result<AcceptedHello, String> {
    let expected = proof(
        master_key,
        "dsh-e2ee-client",
        &[json!(hello.access_session_id), json!(hello.client_random_b64)],
    )?;
    let actual = decode_base64_url(&hello.client_proof_b64, Some(32))?;
    let expected_bytes = decode_base64_url(&expected, Some(32))?;
    if !constant_time_eq(&expected_bytes, &actual) {
        return Err("client proof failed".to_owned());
    }
    let server_proof_b64 = proof(
        master_key,
        "dsh-e2ee-server",
        &[json!(hello.access_session_id), json!(hello.client_random_b64), json!(server_random_b64)],
    )?;
    let material = derive_material(
        master_key,
        &hello.access_session_id,
        &hello.client_random_b64,
        server_random_b64,
    )?;
    let payload = serde_json::to_value(ServerHello {
        access_session_id: hello.access_session_id.clone(),
        server_random_b64: server_random_b64.to_owned(),
        server_proof_b64,
    })
    .map_err(|error| error.to_string())?;
    Ok(AcceptedHello { payload, cipher: SecureCipher::server(material, hello.access_session_id) })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
    const CLIENT_RANDOM: &str = "ICEiIyQlJicoKSorLC0uLzAxMjM0NTY3ODk6Ozw9Pj8";
    const SERVER_RANDOM: &str = "QEFCQ0RFRkdISUpLTE1OT1BRUlNUVVZXWFlaW1xdXl8";
    const SESSION: &str = "access_test_vector";

    #[test]
    fn accepts_the_suite_fixed_handshake_vector() {
        let accepted = accept_client_hello_with_server_random(
            MASTER_KEY,
            ClientHello {
                access_session_id: SESSION.to_owned(),
                client_random_b64: CLIENT_RANDOM.to_owned(),
                client_proof_b64: "F3mAmAuR30RnLXq7TMUeMNWZquo8GPHVyCxWycTDW80".to_owned(),
            },
            SERVER_RANDOM,
        )
        .unwrap();
        assert_eq!(
            accepted.payload["serverProofB64"],
            "IXvvgrKVbAjjW-M2rLOmf-blsUwmbgLa6y79lEj1vNA"
        );
    }

    #[test]
    fn rejects_tampering_and_sequence_replay() {
        let mut cipher = SecureCipher::server(
            derive_material(MASTER_KEY, SESSION, CLIENT_RANDOM, SERVER_RANDOM).unwrap(),
            SESSION.to_owned(),
        );
        let sealed = SealedPayload {
            seq: "0".to_owned(),
            ciphertext_b64:
                "voqHzLsUCWC__C-NBr-s0t1AshPpTEwNwSoOrhx_ihApnwkPlwKxr7EX28kmqOjoVQus161QnjXyzjxDil_WXgnkvu0pgQfiGV27QIgL97KPe2X0nv9vlzLUmwMll0ipeUo2IZKgM-Rt_WRa_-TyyL9SEkozijz1Z6HBxk1hfLiwQEb602fzHVGQP4Wh3Q2B41mrL_-AGQ"
                    .to_owned(),
        };
        let opened = cipher.open(&sealed).unwrap();
        assert_eq!(opened["type"], "http_req");
        assert!(cipher.open(&sealed).is_err());

        let mut changed = sealed;
        changed.ciphertext_b64.replace_range(0..1, "A");
        assert!(
            SecureCipher::server(
                derive_material(MASTER_KEY, SESSION, CLIENT_RANDOM, SERVER_RANDOM,).unwrap(),
                SESSION.to_owned(),
            )
            .open(&changed)
            .is_err()
        );
    }
}
