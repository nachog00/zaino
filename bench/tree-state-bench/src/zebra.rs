//! Minimal JSON-RPC client for the correctness cross-check, plus commitment-tree
//! root recomputation.
//!
//! Plain HTTP/1.1 over a TCP socket (zebra's RPC is unencrypted), so no TLS
//! dependency is pulled in. One request per call, `Connection: close`.

use std::error::Error;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use incrementalmerkletree::Hashable;
use serde_json::{json, Value};
use zcash_primitives::merkle_tree::{read_commitment_tree, HashSer};

/// A zebra JSON-RPC endpoint (`http://host:port/`).
pub struct Rpc {
    host: String,
    port: u16,
}

impl Rpc {
    /// Parse an `http://host:port[/]` endpoint.
    pub fn new(url: &str) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let rest = url.strip_prefix("http://").ok_or("ZEBRA_RPC must start with http://")?;
        let authority = rest.split('/').next().unwrap_or(rest);
        let (host, port) = authority.rsplit_once(':').ok_or("ZEBRA_RPC must be host:port")?;
        Ok(Self { host: host.to_owned(), port: port.parse()? })
    }

    /// One JSON-RPC call, returning the `result` field.
    pub fn call(&self, method: &str, params: Value) -> Result<Value, Box<dyn Error + Send + Sync>> {
        let body = json!({ "jsonrpc": "1.0", "id": "bench", "method": method, "params": params });
        let body = serde_json::to_vec(&body)?;

        let mut stream = TcpStream::connect((self.host.as_str(), self.port))?;
        stream.set_read_timeout(Some(Duration::from_secs(120)))?;
        stream.set_write_timeout(Some(Duration::from_secs(30)))?;

        let request = format!(
            "POST / HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            self.host,
            self.port,
            body.len(),
        );
        stream.write_all(request.as_bytes())?;
        stream.write_all(&body)?;
        stream.flush()?;

        let mut response = Vec::new();
        stream.read_to_end(&mut response)?;

        let split = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or("no header/body split in RPC response")?;
        let payload = &response[split + 4..];
        let parsed: Value = serde_json::from_slice(payload)?;
        if let Some(error) = parsed.get("error") {
            if !error.is_null() {
                return Err(format!("RPC error from {method}: {error}").into());
            }
        }
        parsed.get("result").cloned().ok_or_else(|| "no result field".into())
    }

    /// `getblock <height> 1` → (finalsaplingroot, finalorchardroot) as the RPC
    /// prints them (absent fields = `None`).
    pub fn block_roots(
        &self,
        height: u32,
    ) -> Result<(Option<String>, Option<String>), Box<dyn Error + Send + Sync>> {
        let result = self.call("getblock", json!([height.to_string(), 1]))?;
        let field = |name: &str| result.get(name).and_then(Value::as_str).map(str::to_owned);
        Ok((field("finalsaplingroot"), field("finalorchardroot")))
    }

    /// `z_getsubtreesbyindex <pool> 0 <limit>` → (root hex, end_height) per entry.
    pub fn subtrees(
        &self,
        pool: &str,
        limit: u64,
    ) -> Result<Vec<(String, u32)>, Box<dyn Error + Send + Sync>> {
        let result = self.call("z_getsubtreesbyindex", json!([pool, 0, limit]))?;
        let entries = result
            .get("subtrees")
            .and_then(Value::as_array)
            .ok_or("z_getsubtreesbyindex: no subtrees array")?;
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            let root = entry.get("root").and_then(Value::as_str).ok_or("subtree: no root")?;
            let end =
                entry.get("end_height").and_then(Value::as_u64).ok_or("subtree: no end_height")?;
            out.push((root.to_owned(), u32::try_from(end).map_err(|_| "end_height > u32")?));
        }
        Ok(out)
    }
}

/// The root of one pool's serialized commitment tree, in internal byte order.
fn root_internal<H: HashSer + Hashable + Clone>(
    bytes: &[u8],
) -> Result<[u8; 32], Box<dyn Error + Send + Sync>> {
    let tree = read_commitment_tree::<H, _, 32>(bytes)?;
    let root = tree.to_frontier().root();
    let mut out = [0u8; 32];
    root.write(&mut out[..])?;
    Ok(out)
}

/// Sapling tree root of a `CommitmentTreeBytes` slice, internal byte order.
pub fn sapling_root(bytes: &[u8]) -> Result<[u8; 32], Box<dyn Error + Send + Sync>> {
    root_internal::<sapling_crypto::Node>(bytes)
}

/// Orchard tree root of a `CommitmentTreeBytes` slice, internal byte order.
pub fn orchard_root(bytes: &[u8]) -> Result<[u8; 32], Box<dyn Error + Send + Sync>> {
    root_internal::<orchard::tree::MerkleHashOrchard>(bytes)
}

/// Does `zebra_hex` equal `internal` in either orientation? Returns the matching
/// orientation label, or `None`. The RPC prints roots byte-reversed from the
/// internal `HashSer` form (as it does block hashes), but the exact convention
/// differs between `getblock` roots and `z_getsubtreesbyindex` roots, so both
/// orientations are tried and the matching one reported.
pub fn orientation_match(zebra_hex: &str, internal: &[u8; 32]) -> Option<&'static str> {
    let forward = hex::encode(internal);
    let mut reversed_bytes = *internal;
    reversed_bytes.reverse();
    let reversed = hex::encode(reversed_bytes);
    let zebra = zebra_hex.trim().to_ascii_lowercase();
    if zebra == reversed {
        Some("reversed")
    } else if zebra == forward {
        Some("internal")
    } else {
        None
    }
}
