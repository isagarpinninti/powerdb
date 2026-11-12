//! The command-line side of buckets and objects: `powerdb bucket ...` and `powerdb object ...` talk to a running server over HTTP, so they work the same
//! against a local server, a remote one and a server behind TLS. Files stream in both directions; nothing is read into memory whole.

use crate::error::{Error, Result};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

pub struct Client {
    base: String,
    key: String,
}

fn fail(m: impl Into<String>) -> Error {
    Error::Cli(m.into())
}

/// `bucket/key` into its two parts. The key may contain slashes; the bucket may not.
pub fn split_path(p: &str) -> Result<(&str, &str)> {
    match p.split_once('/') {
        Some((b, k)) if !b.is_empty() && !k.is_empty() => Ok((b, k)),
        _ => Err(fail(format!(
            "\"{p}\" is not BUCKET/KEY (for example documents/contracts/acme.pdf)"
        ))),
    }
}

/// Percent-encodes everything in a key except letters, digits, `-_.~` and the slashes that separate its parts.
fn encode_key(k: &str) -> String {
    k.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Percent-encodes one path or query component.
pub fn encode_component(s: &str) -> String {
    encode_q(s)
}

fn encode_q(s: &str) -> String {
    encode_key(s).replace('/', "%2F")
}

impl Client {
    pub fn new(url: &str, key: Option<String>) -> Result<Self> {
        let key = key.ok_or_else(|| {
            fail("no API key: pass --key or set POWERDB_KEY (an administrator key; `powerdb init` printed one)")
        })?;
        Ok(Self {
            base: format!("{}/api/v1", url.trim_end_matches('/')),
            key,
        })
    }

    fn agent(timeout: Option<Duration>) -> ureq::Agent {
        ureq::Agent::config_builder()
            .timeout_global(timeout)
            .http_status_as_error(false)
            .user_agent("powerdb-cli")
            .build()
            .into()
    }

    fn auth(&self) -> String {
        format!("Bearer {}", self.key)
    }

    /// Turns an error answer into a readable message (the server's own text when it sent one).
    fn explain(mut resp: ureq::http::Response<ureq::Body>) -> Error {
        let status = resp.status();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        let msg = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
            .unwrap_or(text);
        fail(format!("{} ({})", msg.trim(), status.as_u16()))
    }

    fn json_call(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let agent = Self::agent(Some(Duration::from_secs(60)));
        let url = format!("{}{path}", self.base);
        let resp = match (method, body) {
            ("GET", _) => agent.get(&url).header("authorization", self.auth()).call(),
            ("DELETE", _) => agent
                .delete(&url)
                .header("authorization", self.auth())
                .call(),
            (m, Some(b)) => {
                let req = if m == "PATCH" {
                    agent.patch(&url)
                } else {
                    agent.post(&url)
                };
                req.header("authorization", self.auth())
                    .header("content-type", "application/json")
                    .send(b.to_string())
            }
            (_, None) => agent
                .post(&url)
                .header("authorization", self.auth())
                .send(""),
        }
        .map_err(|e| fail(format!("could not reach the server at {}: {e}", self.base)))?;
        if !resp.status().is_success() {
            return Err(Self::explain(resp));
        }
        let text = resp.into_body().read_to_string().unwrap_or_default();
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// Any JSON call (the table commands pass JSON through as it is).
    pub fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        self.json_call(method, path, body)
    }

    // ---- buckets ----

    pub fn bucket_create(&self, name: &str, versioning: bool, encrypt: bool) -> Result<Value> {
        self.json_call(
            "POST",
            "/buckets",
            Some(json!({"name": name, "versioning": versioning, "encrypt": encrypt})),
        )
    }
    pub fn buckets(&self) -> Result<Value> {
        self.json_call("GET", "/buckets", None)
    }
    pub fn bucket_delete(&self, name: &str) -> Result<()> {
        self.json_call("DELETE", &format!("/buckets/{name}"), None)
            .map(|_| ())
    }
    pub fn bucket_versioning(&self, name: &str) -> Result<Value> {
        self.json_call(
            "PATCH",
            &format!("/buckets/{name}"),
            Some(json!({"versioning": true})),
        )
    }

    // ---- objects ----

    /// Uploads `from` (a file, or standard input for `-`) as the object. Streams; the declared type is `mime`.
    pub fn object_put(
        &self,
        bucket: &str,
        key: &str,
        from: &Path,
        mime: Option<&str>,
        meta: &[(String, String)],
    ) -> Result<Value> {
        let guessed;
        let mime = match mime {
            Some(m) => m,
            None => {
                guessed = mime_guess::from_path(from)
                    .first_or_octet_stream()
                    .to_string();
                &guessed
            }
        };
        let name = from.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let mut url = format!("{}/buckets/{bucket}/objects/{}", self.base, encode_key(key));
        if !name.is_empty() && from != Path::new("-") {
            url.push_str(&format!("?filename={}", encode_q(name)));
        }
        let agent = Self::agent(None); // a large upload may take as long as it takes
        let mut req = agent
            .put(&url)
            .header("authorization", self.auth())
            .header("content-type", mime);
        for (k, v) in meta {
            req = req.header(format!("x-powerdb-meta-{k}"), v);
        }
        let resp = if from == Path::new("-") {
            req.send(ureq::SendBody::from_reader(&mut std::io::stdin().lock()))
        } else {
            let mut f = std::fs::File::open(from)
                .map_err(|e| fail(format!("cannot read {}: {e}", from.display())))?;
            req.send(ureq::SendBody::from_reader(&mut f))
        }
        .map_err(|e| fail(format!("the upload failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(Self::explain(resp));
        }
        let text = resp.into_body().read_to_string().unwrap_or_default();
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// Downloads the object to `to` (standard output for `-`), streaming. Returns the number of bytes written.
    pub fn object_get(
        &self,
        bucket: &str,
        key: &str,
        version: Option<&str>,
        to: &Path,
    ) -> Result<u64> {
        let mut url = format!("{}/buckets/{bucket}/objects/{}", self.base, encode_key(key));
        if let Some(v) = version {
            url.push_str(&format!("?version={}", encode_q(v)));
        }
        let resp = Self::agent(None)
            .get(&url)
            .header("authorization", self.auth())
            .call()
            .map_err(|e| fail(format!("could not reach the server at {}: {e}", self.base)))?;
        if !resp.status().is_success() {
            return Err(Self::explain(resp));
        }
        let mut body = resp.into_body();
        let mut reader = body.as_reader();
        if to == Path::new("-") {
            let mut out = std::io::stdout().lock();
            let n = std::io::copy(&mut reader, &mut out)?;
            out.flush()?;
            return Ok(n);
        }
        // written beside the target and moved into place, so an interrupted download never leaves a short file that looks whole
        let tmp = to.with_extension(format!(
            "{}part",
            to.extension()
                .and_then(|e| e.to_str())
                .map(|e| format!("{e}."))
                .unwrap_or_default()
        ));
        let n = {
            let mut f = std::fs::File::create(&tmp)?;
            let mut buf = vec![0u8; 1 << 16];
            let mut n = 0u64;
            loop {
                let r = reader.read(&mut buf).map_err(|e| {
                    let _ = std::fs::remove_file(&tmp);
                    fail(format!("the download was interrupted: {e}"))
                })?;
                if r == 0 {
                    break;
                }
                f.write_all(&buf[..r])?;
                n += r as u64;
            }
            f.sync_all()?;
            n
        };
        std::fs::rename(&tmp, to)?;
        Ok(n)
    }

    pub fn object_delete(&self, bucket: &str, key: &str, version: Option<&str>) -> Result<()> {
        let mut path = format!("/buckets/{bucket}/objects/{}", encode_key(key));
        if let Some(v) = version {
            path.push_str(&format!("?version={}", encode_q(v)));
        }
        self.json_call("DELETE", &path, None).map(|_| ())
    }

    pub fn objects(&self, bucket: &str, prefix: &str, after: Option<&str>) -> Result<Value> {
        let mut path = format!(
            "/buckets/{bucket}/objects?limit=1000&prefix={}",
            encode_q(prefix)
        );
        if let Some(a) = after {
            path.push_str(&format!("&after={}", encode_q(a)));
        }
        self.json_call("GET", &path, None)
    }

    pub fn versions(&self, bucket: &str, key: &str) -> Result<Value> {
        self.json_call(
            "GET",
            &format!("/buckets/{bucket}/versions?key={}", encode_q(key)),
            None,
        )
    }

    pub fn restore(&self, bucket: &str, key: &str, version: &str) -> Result<Value> {
        self.json_call(
            "POST",
            &format!("/buckets/{bucket}/restore"),
            Some(json!({"key": key, "version": version})),
        )
    }
}
