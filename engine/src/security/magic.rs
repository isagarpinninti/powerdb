//! Content sniffing on the first bytes of an upload, before anything is written. It stops a file whose bytes do not match the type it claims (a Windows
//! program named `photo.png`, a script declared as `application/pdf`) and refuses executable formats outright.
//!
//! What it does not do: detect malware. A document with a malicious macro, or a polyglot that is valid as two formats, passes. Treat it as a gate against
//! disguised executables and mislabelled files, not as a scanner. Files are also served with `X-Content-Type-Options: nosniff` and a sandboxing CSP.

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Image,
    Audio,
    Video,
    Document,
    Archive,
    Text,
    Executable,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sniffed {
    pub mime: &'static str,
    pub kind: Kind,
}

const fn s(mime: &'static str, kind: Kind) -> Sniffed {
    Sniffed { mime, kind }
}

/// How many leading bytes the caller should hand to `sniff`.
pub const HEAD_BYTES: usize = 512;

pub fn sniff(h: &[u8]) -> Sniffed {
    let starts = |p: &[u8]| h.starts_with(p);
    if starts(b"\x7fELF")
        || starts(b"MZ")
        || starts(&[0xFE, 0xED, 0xFA, 0xCE])
        || starts(&[0xFE, 0xED, 0xFA, 0xCF])
        || starts(&[0xCE, 0xFA, 0xED, 0xFE])
        || starts(&[0xCF, 0xFA, 0xED, 0xFE])
        || starts(&[0xCA, 0xFE, 0xBA, 0xBE])
    {
        return s("application/x-executable", Kind::Executable);
    }
    if starts(b"#!") {
        return s("text/x-script", Kind::Executable);
    }
    if starts(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return s("image/png", Kind::Image);
    }
    if starts(&[0xFF, 0xD8, 0xFF]) {
        return s("image/jpeg", Kind::Image);
    }
    if starts(b"GIF87a") || starts(b"GIF89a") {
        return s("image/gif", Kind::Image);
    }
    if starts(b"RIFF") && h.len() >= 12 {
        match &h[8..12] {
            b"WEBP" => return s("image/webp", Kind::Image),
            b"WAVE" => return s("audio/wav", Kind::Audio),
            b"AVI " => return s("video/x-msvideo", Kind::Video),
            _ => {}
        }
    }
    if starts(b"II*\0") || starts(b"MM\0*") {
        return s("image/tiff", Kind::Image);
    }
    if starts(&[0, 0, 1, 0]) {
        return s("image/x-icon", Kind::Image);
    }
    if starts(b"%PDF-") {
        return s("application/pdf", Kind::Document);
    }
    if starts(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        return s("application/x-ole-storage", Kind::Document);
    }
    if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") {
        return s("application/zip", Kind::Archive);
    }
    if starts(&[0x1F, 0x8B]) {
        return s("application/gzip", Kind::Archive);
    }
    if starts(b"BZh") {
        return s("application/x-bzip2", Kind::Archive);
    }
    if starts(&[0xFD, b'7', b'z', b'X', b'Z', 0]) {
        return s("application/x-xz", Kind::Archive);
    }
    if starts(&[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) {
        return s("application/x-7z-compressed", Kind::Archive);
    }
    if starts(b"Rar!\x1a\x07") {
        return s("application/vnd.rar", Kind::Archive);
    }
    if h.len() >= 12 && &h[4..8] == b"ftyp" {
        return s("video/mp4", Kind::Video);
    }
    if starts(b"ID3") || starts(&[0xFF, 0xFB]) || starts(&[0xFF, 0xF3]) || starts(&[0xFF, 0xF2]) {
        return s("audio/mpeg", Kind::Audio);
    }
    if starts(b"OggS") {
        return s("audio/ogg", Kind::Audio);
    }
    if starts(b"fLaC") {
        return s("audio/flac", Kind::Audio);
    }
    if starts(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return s("video/webm", Kind::Video);
    }
    // text: valid UTF-8 (allowing a multi-byte character cut by the end of the window), no NUL, mostly printable
    let valid = match std::str::from_utf8(h) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none() && e.valid_up_to() + 4 > h.len(),
    };
    if valid && !h.is_empty() && !h.contains(&0) {
        let printable = h
            .iter()
            .filter(|b| **b >= 0x20 || matches!(**b, b'\n' | b'\r' | b'\t') || **b >= 0x80)
            .count();
        if printable * 100 >= h.len() * 95 {
            let t = String::from_utf8_lossy(h);
            let t = t.trim_start_matches('\u{feff}').trim_start();
            if t.starts_with("<svg") || (t.starts_with("<?xml") && t.contains("<svg")) {
                return s("image/svg+xml", Kind::Text);
            }
            return s("text/plain", Kind::Text);
        }
    }
    s("application/octet-stream", Kind::Unknown)
}

fn base(mime: &str) -> String {
    mime.split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn is_zip_family(m: &str) -> bool {
    m == "application/zip"
        || m == "application/x-zip-compressed"
        || m == "application/java-archive"
        || m == "application/epub+zip"
        || m.starts_with("application/vnd.openxmlformats-officedocument.")
        || m.starts_with("application/vnd.oasis.opendocument.")
}

fn is_text_like(m: &str) -> bool {
    m.starts_with("text/")
        || matches!(
            m,
            "application/json"
                | "application/xml"
                | "application/javascript"
                | "application/x-ndjson"
                | "application/yaml"
                | "image/svg+xml"
                | "application/csv"
        )
}

fn compatible(declared: &str, sniffed: &Sniffed) -> bool {
    let d = declared;
    match sniffed.kind {
        Kind::Text => is_text_like(d),
        _ => {
            d == sniffed.mime
                || (sniffed.mime == "application/zip" && is_zip_family(d))
                || (sniffed.mime == "image/jpeg" && d == "image/jpg")
                || (sniffed.mime == "video/mp4"
                    && (d == "audio/mp4"
                        || d == "video/quicktime"
                        || d == "audio/x-m4a"
                        || d == "video/x-m4v"))
                || (sniffed.mime == "audio/wav" && (d == "audio/x-wav" || d == "audio/wave"))
                || (sniffed.mime == "application/x-ole-storage"
                    && (d == "application/msword"
                        || d == "application/vnd.ms-excel"
                        || d == "application/vnd.ms-powerpoint"))
                || (sniffed.mime == "audio/ogg" && (d == "video/ogg" || d == "application/ogg"))
                || (sniffed.mime == "application/gzip" && d == "application/x-gzip")
        }
    }
}

/// Decides whether `head` (the first bytes of the upload) is acceptable for a file declared as `declared`, and returns the content type to store.
pub fn check(declared: &str, head: &[u8]) -> Result<String> {
    let d = base(declared);
    let sn = sniff(head);
    if sn.kind == Kind::Executable {
        return Err(Error::Invalid("executable files are not accepted".into()));
    }
    if d.is_empty() || d == "application/octet-stream" || d == "binary/octet-stream" {
        return Ok(if sn.kind == Kind::Unknown {
            "application/octet-stream".into()
        } else {
            sn.mime.to_string()
        });
    }
    match sn.kind {
        Kind::Unknown => {
            let binary_family = d.starts_with("image/")
                || d.starts_with("audio/")
                || d.starts_with("video/")
                || d == "application/pdf"
                || is_zip_family(&d)
                || d == "application/gzip";
            if head.is_empty() && !binary_family {
                return Ok(d);
            }
            if binary_family || is_text_like(&d) {
                return Err(Error::Invalid(format!(
                    "the file is declared as {d} but its content does not look like one"
                )));
            }
            Ok(d) // some other binary type we have no signature for: trust the declaration, the executable gate above has already run
        }
        _ => {
            if compatible(&d, &sn) {
                Ok(d)
            } else {
                Err(Error::Invalid(format!(
                    "the file is declared as {d} but its content looks like {}",
                    sn.mime
                )))
            }
        }
    }
}

/// Content types that a browser could run as a page or script from our origin. These are always served as downloads.
pub fn must_download(mime: &str) -> bool {
    let m = base(mime);
    !(m.starts_with("image/") && m != "image/svg+xml"
        || m.starts_with("audio/")
        || m.starts_with("video/")
        || m == "application/pdf"
        || m == "text/plain")
}
