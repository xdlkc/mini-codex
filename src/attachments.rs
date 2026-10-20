//! Private, persistent local attachments. Parsers never execute document content.

use base64::{engine::general_purpose::STANDARD, Engine};
use quick_xml::{events::Event, Reader};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Cursor, Read, Write},
    path::{Component, Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, String>;
pub const MAX_FILE_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_ZIP_BYTES: usize = 64 * 1024 * 1024;
const MAX_ENTRY_BYTES: usize = 16 * 1024 * 1024;
const MAX_ZIP_ENTRIES: usize = 4096;
const MAX_XML_NODES: usize = 200_000;
static STORE_MUTEX: Mutex<()> = Mutex::new(());

/// JSONL RPC entry point. Parsing and disk I/O run outside the async executor.
pub async fn handle(method: &str, params: &Value) -> Option<Result<Value>> {
    if !matches!(
        method,
        "files/import" | "files/read" | "files/list" | "files/delete"
    ) {
        return None;
    }
    let method = method.to_owned();
    let params = params.clone();
    Some(
        tokio::task::spawn_blocking(move || Store::configured().rpc(&method, &params))
            .await
            .map_err(|_| "Attachment worker stopped unexpectedly".to_string())
            .and_then(|r| r),
    )
}

/// Complete Responses input. Failed or textless documents cannot masquerade as read.
pub fn model_content(id: &str) -> Result<Vec<Value>> {
    Store::configured().with_lock(|store| store.model_content(id))
}

/// Stable metadata for a restored session. Contains no local storage paths.
pub fn read_metadata(id: &str) -> Result<Value> {
    Store::configured().with_lock(|store| Ok(serde_json::to_value(store.metadata(id)?).unwrap()))
}

/// Original image bytes for provider edits. Caller receives no storage path.
pub(crate) fn read_image_original(id: &str) -> Result<(Value, Vec<u8>)> {
    Store::configured().with_lock(|store| store.image_original(id))
}

/// Commit a generated image through the same validation and private store as imports.
pub(crate) fn import_generated_image(name: &str, mime_type: &str, bytes: &[u8]) -> Result<Value> {
    Store::configured().with_lock(|store| store.import_image(name, mime_type, bytes))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Metadata {
    id: String,
    name: String,
    mime_type: String,
    size: usize,
    sha256: String,
    kind: String,
    created_at: u64,
    parse_status: String,
    text_bytes: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    page_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sheet_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
    warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Default)]
struct Parsed {
    text: Option<String>,
    has_document_text: Option<bool>,
    page_count: Option<usize>,
    sheet_count: Option<usize>,
    dimensions: Option<(u32, u32)>,
    warnings: Vec<String>,
    error: Option<String>,
}

#[derive(Clone)]
struct Store {
    root: PathBuf,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn random_id() -> String {
    format!("att_{:032x}", rand::random::<u128>())
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn io_error(action: &str, e: std::io::Error) -> String {
    format!("{action}: {e}")
}
fn limit(message: &str) -> String {
    format!("Attachment limit exceeded: {message}")
}
fn is_limit(e: &str) -> bool {
    e.starts_with("Attachment limit exceeded:")
}
fn valid_id(id: &str) -> bool {
    id.len() == 36 && id.starts_with("att_") && id[4..].bytes().all(|b| b.is_ascii_hexdigit())
}
fn id_param(params: &Value) -> Result<&str> {
    let id = params["id"].as_str().ok_or("id is required")?;
    if !valid_id(id) {
        return Err("Invalid attachment id".into());
    }
    Ok(id)
}
fn flag(params: &Value, name: &str) -> Result<bool> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        _ => Err(format!("{name} must be boolean")),
    }
}

fn private_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
        Ok(_) => return Err("Attachment storage is not a private directory".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut b = fs::DirBuilder::new();
            b.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                b.mode(0o700);
            }
            b.create(path)
                .map_err(|e| io_error("Cannot create attachment storage", e))?;
        }
        Err(e) => return Err(io_error("Cannot inspect attachment storage", e)),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|e| io_error("Cannot protect attachment storage", e))?;
    }
    Ok(())
}

fn new_private_file(path: &Path) -> Result<File> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
        .map_err(|e| io_error("Cannot create attachment file", e))
}
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = new_private_file(path)?;
    f.write_all(bytes)
        .and_then(|_| f.sync_all())
        .map_err(|e| io_error("Cannot save attachment file", e))
}
fn read_private(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    let m =
        fs::symlink_metadata(path).map_err(|e| io_error("Cannot inspect attachment file", e))?;
    if !m.is_file() || m.file_type().is_symlink() {
        return Err("Attachment storage contains an unsafe file".into());
    }
    if m.len() > maximum as u64 {
        return Err(limit("stored attachment file is too large"));
    }
    let mut out = Vec::new();
    File::open(path)
        .map_err(|e| io_error("Cannot open attachment file", e))?
        .take(maximum as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| io_error("Cannot read attachment file", e))?;
    if out.len() > maximum {
        return Err(limit("stored attachment file is too large"));
    }
    Ok(out)
}

impl Store {
    fn configured() -> Self {
        Self {
            root: crate::config::home().join("attachments"),
        }
    }
    fn with_lock<T>(&self, f: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        let _guard = STORE_MUTEX
            .lock()
            .map_err(|_| "Attachment storage lock poisoned")?;
        private_dir(&self.root)?;
        let lock_path = self.root.join(".lock");
        if let Ok(m) = fs::symlink_metadata(&lock_path) {
            if !m.is_file() || m.file_type().is_symlink() {
                return Err("Unsafe attachment storage lock".into());
            }
        }
        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let lock = opts
            .open(lock_path)
            .map_err(|e| io_error("Cannot open attachment storage lock", e))?;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err("Attachment storage is busy".into())
                }
                Err(e) => return Err(format!("Cannot lock attachment storage: {e}")),
            }
        }
        f(self)
    }
    fn dir(&self, id: &str) -> Result<PathBuf> {
        if !valid_id(id) {
            return Err("Invalid attachment id".into());
        }
        let path = self.root.join(id);
        let m = fs::symlink_metadata(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "Attachment not found".into()
            } else {
                io_error("Cannot inspect attachment", e)
            }
        })?;
        if !m.is_dir() || m.file_type().is_symlink() {
            return Err("Unsafe attachment directory".into());
        }
        Ok(path)
    }
    fn metadata(&self, id: &str) -> Result<Metadata> {
        let path = self.dir(id)?;
        let m: Metadata =
            serde_json::from_slice(&read_private(&path.join("metadata.json"), 64 * 1024)?)
                .map_err(|_| "Invalid attachment metadata")?;
        if m.id != id
            || m.size > MAX_FILE_BYTES
            || m.text_bytes > MAX_TEXT_BYTES
            || !matches!(m.parse_status.as_str(), "ready" | "no_text" | "failed")
        {
            return Err("Invalid attachment metadata".into());
        }
        Ok(m)
    }
    fn original(&self, m: &Metadata) -> Result<Vec<u8>> {
        let bytes = read_private(&self.dir(&m.id)?.join("original.bin"), MAX_FILE_BYTES)?;
        if bytes.len() != m.size || hash(&bytes) != m.sha256 {
            return Err("Attachment original failed integrity verification".into());
        }
        Ok(bytes)
    }
    fn image_original(&self, id: &str) -> Result<(Value, Vec<u8>)> {
        let m = self.metadata(id)?;
        if m.kind != "image" || m.parse_status != "ready" {
            return Err("Choose a ready image from the file library".into());
        }
        let bytes = self.original(&m)?;
        let (kind, mime) = detect_type(&m.name, Some(&m.mime_type), &bytes)?;
        if kind != "image" || mime != m.mime_type {
            return Err("Stored attachment is not a supported image".into());
        }
        parse(&kind, &bytes)?;
        Ok((serde_json::to_value(m).unwrap(), bytes))
    }
    fn import_image(&self, name: &str, mime: &str, bytes: &[u8]) -> Result<Value> {
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(limit("image exceeds 10 MiB"));
        }
        let (kind, detected) = detect_type(name, Some(mime), bytes)?;
        if kind != "image" || detected != mime {
            return Err("Generated output is not a supported image".into());
        }
        // Reject incomplete or invalid provider output before writing a library entry.
        parse(&kind, bytes)?;
        let value =
            self.import(&json!({"name":name,"mimeType":mime,"dataBase64":STANDARD.encode(bytes)}))?;
        Ok(value["attachment"].clone())
    }
    fn text(&self, m: &Metadata) -> Result<String> {
        let bytes = read_private(&self.dir(&m.id)?.join("text.txt"), MAX_TEXT_BYTES)?;
        if bytes.len() != m.text_bytes || m.text_sha256.as_deref() != Some(hash(&bytes).as_str()) {
            return Err("Attachment text failed integrity verification".into());
        }
        String::from_utf8(bytes).map_err(|_| "Stored attachment text is invalid UTF-8".into())
    }
    fn model_content(&self, id: &str) -> Result<Vec<Value>> {
        let m = self.metadata(id)?;
        if m.parse_status != "ready" {
            return Err(format!(
                "{}: {}",
                m.name,
                m.error
                    .as_deref()
                    .unwrap_or("No extractable text was found; scanned pages require OCR, which is unavailable")
            ));
        }
        if m.kind == "image" {
            let bytes = self.original(&m)?;
            return Ok(vec![
                json!({"type":"input_image","image_url":format!("data:{};base64,{}",m.mime_type,STANDARD.encode(bytes))}),
            ]);
        }
        // JSON quoting prevents a filename from altering the wrapper. It is data.
        let text = self.text(&m)?;
        Ok(vec![
            json!({"type":"input_text","text":format!("<attachment name={} id={} mimeType={}>\n{}\n</attachment>",serde_json::to_string(&m.name).unwrap(),serde_json::to_string(&m.id).unwrap(),serde_json::to_string(&m.mime_type).unwrap(),text)}),
        ])
    }
    fn rpc(&self, method: &str, p: &Value) -> Result<Value> {
        self.with_lock(|store| match method {
            "files/import" => store.import(p),
            "files/read" => {
                let m = store.metadata(id_param(p)?)?;
                let mut result = json!({"attachment":m});
                if flag(p, "includeContent")? {
                    if m.kind == "image" {
                        result["content"] =
                            json!({"dataBase64":STANDARD.encode(store.original(&m)?)});
                    } else if m.text_sha256.is_some() {
                        result["content"] = json!({"text":store.text(&m)?});
                    } else {
                        result["content"] = Value::Null;
                    }
                }
                if flag(p, "includeOriginal")? {
                    if !result["content"].is_object() {
                        result["content"] = json!({});
                    }
                    result["content"]["dataBase64"] = json!(STANDARD.encode(store.original(&m)?));
                }
                Ok(result)
            }
            "files/list" => {
                let mut data = Vec::new();
                for entry in
                    fs::read_dir(&store.root).map_err(|e| io_error("Cannot list attachments", e))?
                {
                    let entry = entry.map_err(|e| io_error("Cannot list attachments", e))?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if valid_id(&name) {
                        data.push(store.metadata(&name)?);
                    }
                }
                data.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
                Ok(json!({"data":data}))
            }
            "files/delete" => {
                let id = id_param(p)?;
                let path = store.dir(id)?;
                // Only our three regular files can be removed; never recurse into a tampered tree.
                for entry in
                    fs::read_dir(&path).map_err(|e| io_error("Cannot inspect attachment", e))?
                {
                    let entry = entry.map_err(|e| io_error("Cannot inspect attachment", e))?;
                    let name = entry.file_name();
                    if !["metadata.json", "original.bin", "text.txt"]
                        .iter()
                        .any(|n| name == *n)
                        || !entry
                            .file_type()
                            .map_err(|e| io_error("Cannot inspect attachment", e))?
                            .is_file()
                    {
                        return Err(
                            "Attachment directory contains unexpected data; deletion refused"
                                .into(),
                        );
                    }
                }
                let trash = store.root.join(format!(".deleted-{}", random_id()));
                fs::rename(path, &trash).map_err(|e| io_error("Cannot delete attachment", e))?;
                fs::remove_dir_all(&trash)
                    .map_err(|e| io_error("Cannot remove attachment files", e))?;
                File::open(&store.root)
                    .and_then(|f| f.sync_all())
                    .map_err(|e| io_error("Cannot sync attachment deletion", e))?;
                Ok(json!({"deleted":true,"id":id}))
            }
            _ => Err("Unknown attachment method".into()),
        })
    }
    fn import(&self, p: &Value) -> Result<Value> {
        let name = p["name"].as_str().ok_or("name is required")?.trim();
        if name.is_empty()
            || name.len() > 255
            || name
                .chars()
                .any(|c| c.is_control() || c == '/' || c == '\\')
        {
            return Err("Attachment name must be a filename of at most 255 UTF-8 bytes".into());
        }
        let encoded = p["dataBase64"].as_str().ok_or("dataBase64 is required")?;
        if encoded.len() > MAX_FILE_BYTES.div_ceil(3) * 4 {
            return Err(limit("original exceeds 20 MiB"));
        }
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| "Invalid base64 attachment data")?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(limit("original exceeds 20 MiB"));
        }
        let supplied = match p.get("mimeType") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.as_str()),
            _ => return Err("mimeType must be a string".into()),
        };
        let (kind, mime) = detect_type(name, supplied, &bytes)?;
        let parsed =
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parse(&kind, &bytes))) {
                Ok(Ok(p)) => p,
                Ok(Err(e)) if is_limit(&e) => return Err(e),
                Ok(Err(e)) => Parsed {
                    error: Some(e),
                    ..Default::default()
                },
                Err(_) => Parsed {
                    error: Some("Document parser could not safely read this file".into()),
                    ..Default::default()
                },
            };
        let status = if parsed.error.is_some() {
            "failed"
        } else if kind == "image"
            || parsed
                .has_document_text
                .unwrap_or_else(|| parsed.text.as_ref().is_some_and(|s| !s.trim().is_empty()))
        {
            "ready"
        } else {
            "no_text"
        };
        let mut warnings = parsed.warnings;
        if status == "no_text" {
            warnings.push(
                if kind == "pdf" {
                    "No extractable PDF text. Scanned pages require OCR, which is not available."
                } else {
                    "No extractable document text was found."
                }
                .into(),
            );
        }
        let id = random_id();
        let m = Metadata {
            id: id.clone(),
            name: name.into(),
            mime_type: mime,
            size: bytes.len(),
            sha256: hash(&bytes),
            kind,
            created_at: now(),
            parse_status: status.into(),
            text_bytes: parsed.text.as_ref().map_or(0, String::len),
            text_sha256: parsed.text.as_ref().map(|s| hash(s.as_bytes())),
            page_count: parsed.page_count,
            sheet_count: parsed.sheet_count,
            width: parsed.dimensions.map(|d| d.0),
            height: parsed.dimensions.map(|d| d.1),
            warnings,
            error: parsed.error,
        };
        let tmp = self.root.join(format!(".import-{}", random_id()));
        private_dir(&tmp)?;
        let saved = (|| {
            write_private(&tmp.join("original.bin"), &bytes)?;
            if let Some(text) = parsed.text {
                write_private(&tmp.join("text.txt"), text.as_bytes())?;
            }
            write_private(&tmp.join("metadata.json"), &serde_json::to_vec(&m).unwrap())?;
            File::open(&tmp)
                .and_then(|f| f.sync_all())
                .map_err(|e| io_error("Cannot sync attachment", e))?;
            if self.root.join(&id).exists() {
                return Err("Attachment id collision; retry import".into());
            }
            fs::rename(&tmp, self.root.join(&id))
                .map_err(|e| io_error("Cannot commit attachment", e))?;
            File::open(&self.root)
                .and_then(|f| f.sync_all())
                .map_err(|e| io_error("Cannot sync attachment storage", e))?;
            Ok(json!({"attachment":m}))
        })();
        if saved.is_err() {
            let _ = fs::remove_dir_all(&tmp);
        }
        saved
    }
}

fn detect_type(name: &str, supplied: Option<&str>, bytes: &[u8]) -> Result<(String, String)> {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let detected = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    };
    let (kind, mime) = if let Some(mime) = detected {
        ("image".into(), mime.into())
    } else if ext == "pdf" || bytes.starts_with(b"%PDF-") {
        ("pdf".into(), "application/pdf".into())
    } else if ext == "docx" {
        (
            "docx".into(),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document".into(),
        )
    } else if ext == "pptx" {
        (
            "pptx".into(),
            "application/vnd.openxmlformats-officedocument.presentationml.presentation".into(),
        )
    } else if ext == "xlsx" {
        (
            "xlsx".into(),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".into(),
        )
    } else if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp") {
        return Err("Image content does not match a supported image format".into());
    } else if supplied.is_some_and(|m| m.starts_with("text/"))
        || matches!(
            ext.as_str(),
            "txt"
                | "text"
                | "csv"
                | "tsv"
                | "json"
                | "jsonl"
                | "md"
                | "markdown"
                | "log"
                | "rs"
                | "js"
                | "ts"
                | "tsx"
                | "jsx"
                | "py"
                | "html"
                | "htm"
                | "css"
                | "toml"
                | "yaml"
                | "yml"
                | "xml"
                | "sql"
                | "sh"
                | "ini"
                | "cfg"
        )
    {
        (
            "text".into(),
            match ext.as_str() {
                "json" => "application/json",
                "csv" => "text/csv",
                "md" | "markdown" => "text/markdown",
                "html" | "htm" => "text/html",
                _ => "text/plain",
            }
            .into(),
        )
    } else {
        return Err("Unsupported file type. Use text, CSV, JSON, Markdown, PDF, DOCX, PPTX, XLSX, PNG, JPEG or WebP.".into());
    };
    if let Some(m) = supplied.filter(|m| !m.is_empty() && *m != "application/octet-stream") {
        let m = m.split(';').next().unwrap().trim();
        if m != mime
            && !(kind == "text"
                && (m.starts_with("text/")
                    || matches!(
                        m,
                        "application/json"
                            | "application/xml"
                            | "application/javascript"
                            | "application/x-javascript"
                            | "application/x-ndjson"
                            | "application/jsonlines"
                            | "application/x-yaml"
                            | "application/yaml"
                            | "application/toml"
                            | "application/sql"
                    )))
            && !(mime == "image/jpeg" && m == "image/jpg")
        {
            return Err("Declared MIME type does not match attachment content".into());
        }
    }
    Ok((kind, mime))
}

fn decode_text(bytes: &[u8]) -> Result<String> {
    decode_string(bytes, MAX_TEXT_BYTES)
}
fn decode_string(bytes: &[u8], maximum: usize) -> Result<String> {
    let text = if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        if (bytes.len() - 2) % 2 != 0 {
            return Err("Invalid UTF-16 text".into());
        }
        let little = bytes[0] == 0xff;
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| {
                if little {
                    u16::from_le_bytes([c[0], c[1]])
                } else {
                    u16::from_be_bytes([c[0], c[1]])
                }
            })
            .collect();
        String::from_utf16(&units).map_err(|_| "Invalid UTF-16 text")?
    } else {
        std::str::from_utf8(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes))
            .map_err(|_| "Text must use UTF-8 or BOM-marked UTF-16")?
            .to_owned()
    };
    if text
        .chars()
        .any(|c| c == '\0' || (c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\u{000c}')))
    {
        return Err("File contains binary control characters".into());
    }
    if text.len() > maximum {
        return Err(limit(if maximum == MAX_TEXT_BYTES {
            "extracted text exceeds 8 MiB"
        } else {
            "Office XML text exceeds 16 MiB"
        }));
    }
    Ok(text)
}

fn parse(kind: &str, bytes: &[u8]) -> Result<Parsed> {
    match kind {
        "text" => Ok(Parsed {
            text: Some(decode_text(bytes)?),
            ..Default::default()
        }),
        "image" => {
            if bytes.len() > MAX_IMAGE_BYTES {
                return Err(limit("image exceeds 10 MiB"));
            }
            let mut reader = image::ImageReader::new(Cursor::new(bytes))
                .with_guessed_format()
                .map_err(|_| "Cannot identify image")?;
            let dimensions = image::ImageReader::new(Cursor::new(bytes))
                .with_guessed_format()
                .map_err(|_| "Cannot identify image")?
                .into_dimensions()
                .map_err(|e| format!("Cannot read image dimensions: {e}"))?;
            if dimensions.0 > 8192
                || dimensions.1 > 8192
                || u64::from(dimensions.0) * u64::from(dimensions.1) > 32_000_000
            {
                return Err(limit(
                    "image exceeds 8192 pixels per edge or 32 million total pixels",
                ));
            }
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(8192);
            limits.max_image_height = Some(8192);
            limits.max_alloc = Some(256 * 1024 * 1024);
            reader.limits(limits);
            let img = reader.decode().map_err(|e| {
                if matches!(e, image::ImageError::Limits(_)) {
                    limit("image decoding exceeds its memory budget")
                } else {
                    format!("Cannot decode image: {e}")
                }
            })?;
            if u64::from(img.width()) * u64::from(img.height()) > 32_000_000 {
                return Err(limit("image exceeds 32 million pixels"));
            }
            Ok(Parsed {
                dimensions: Some((img.width(), img.height())),
                ..Default::default()
            })
        }
        "pdf" => parse_pdf(bytes),
        "docx" | "pptx" | "xlsx" => parse_office(kind, bytes),
        _ => Err("Unsupported attachment parser".into()),
    }
}

fn parse_pdf(bytes: &[u8]) -> Result<Parsed> {
    let options = lopdf::LoadOptions {
        strict: true,
        max_decompressed_size: Some(MAX_ENTRY_BYTES),
        ..Default::default()
    };
    let doc = lopdf::Document::load_mem_with_options(bytes, options)
        .map_err(|e| pdf_error("Cannot parse PDF", e))?;
    if doc.is_encrypted() {
        return Err("Password-protected PDF cannot be read. Import an unlocked copy.".into());
    }
    if doc.objects.len() > 100_000 {
        return Err(limit("PDF exceeds 100000 objects"));
    }
    let pages = doc.get_pages();
    if pages.len() > 1000 {
        return Err(limit("PDF exceeds 1000 pages"));
    }
    let mut text = String::new();
    let mut budget = MAX_ZIP_BYTES;
    for (number, id) in &pages {
        let content = doc
            .get_page_content_with_limit(*id, budget.min(MAX_ENTRY_BYTES))
            .map_err(|e| pdf_error(&format!("Cannot decode PDF page {number}"), e))?;
        budget = budget
            .checked_sub(content.len())
            .ok_or_else(|| limit("PDF page data exceeds 64 MiB"))?;
        let page = doc
            .extract_text_with_limit(&[*number], MAX_ENTRY_BYTES)
            .map_err(|e| pdf_error(&format!("Cannot extract PDF page {number}"), e))?;
        if text.len() + page.len() + 32 > MAX_TEXT_BYTES {
            return Err(limit("extracted text exceeds 8 MiB"));
        }
        if !page.trim().is_empty() {
            text.push_str(&format!("[Page {number}]\n{page}\n"));
        }
    }
    Ok(Parsed {
        text: Some(text),
        page_count: Some(pages.len()),
        warnings: vec![
            "PDF text only; images, scanned pages and visual layout are not read by OCR.".into(),
        ],
        ..Default::default()
    })
}
fn pdf_error(context: &str, e: lopdf::Error) -> String {
    match e {
        lopdf::Error::Decompress(lopdf::DecompressError::MemoryLimitExceeded { .. }) => {
            limit("PDF stream decompression exceeds its byte budget")
        }
        lopdf::Error::InvalidPassword | lopdf::Error::Decryption(_) => {
            "Password-protected PDF cannot be read. Import an unlocked copy.".into()
        }
        other => format!("{context}: {other}"),
    }
}

#[derive(Debug, Default)]
struct Node {
    name: String,
    attrs: BTreeMap<String, String>,
    text: String,
    children: Vec<Node>,
}
impl Node {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.get(name).map(String::as_str)
    }
    fn relationship_id(&self) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k.ends_with(":id"))
            .map(|(_, v)| v.as_str())
    }
    fn descendants<'a>(&'a self, name: &str, out: &mut Vec<&'a Node>) {
        if self.name == name {
            out.push(self);
        }
        for child in &self.children {
            child.descendants(name, out);
        }
    }
    fn all<'a>(&'a self, name: &str) -> Vec<&'a Node> {
        let mut out = Vec::new();
        self.descendants(name, &mut out);
        out
    }
    fn value(&self) -> String {
        let mut out = self.text.clone();
        for c in &self.children {
            out.push_str(&c.value());
        }
        out
    }
}
fn local_name(s: &str) -> String {
    s.rsplit(':').next().unwrap_or(s).into()
}
fn xml(bytes: &[u8]) -> Result<Node> {
    let source = decode_string(bytes, MAX_ENTRY_BYTES)?;
    let mut reader = Reader::from_str(&source);
    let mut stack = vec![Node {
        name: "#document".into(),
        ..Default::default()
    }];
    let mut count = 0usize;
    loop {
        match reader
            .read_event()
            .map_err(|e| format!("Invalid Office XML: {e}"))?
        {
            event @ (Event::Start(_) | Event::Empty(_)) => {
                let empty = matches!(event, Event::Empty(_));
                let e = match event {
                    Event::Start(e) | Event::Empty(e) => e,
                    _ => unreachable!(),
                };
                let mut node = Node {
                    name: local_name(e.name().as_ref()),
                    ..Default::default()
                };
                for a in e.attributes() {
                    let a = a.map_err(|_| "Invalid XML attribute")?;
                    let name = a.key.as_ref().to_owned();
                    let val = a
                        .normalized_value(quick_xml::XmlVersion::default())
                        .map_err(|_| "Invalid XML attribute value")?
                        .into_owned();
                    if node.attrs.insert(name, val).is_some() {
                        return Err("Duplicate XML attribute".into());
                    }
                }
                count += 1;
                if count > MAX_XML_NODES || stack.len() > 96 {
                    return Err(limit("Office XML has too many nodes or excessive nesting"));
                }
                if empty {
                    stack.last_mut().unwrap().children.push(node);
                } else {
                    stack.push(node);
                }
            }
            Event::End(_) => {
                if stack.len() < 2 {
                    return Err("Invalid XML closing element".into());
                }
                let node = stack.pop().unwrap();
                stack.last_mut().unwrap().children.push(node);
            }
            Event::Text(e) => stack.last_mut().unwrap().text.push_str(&e.xml10_content()),
            Event::CData(e) => stack.last_mut().unwrap().text.push_str(e.as_ref()),
            Event::GeneralRef(e) => {
                let raw = e.as_ref();
                let entity = format!("&{raw};");
                let value =
                    quick_xml::escape::unescape(&entity).map_err(|_| "Unsupported XML entity")?;
                stack.last_mut().unwrap().text.push_str(&value);
            }
            Event::DocType(_) => {
                return Err("XML document types and external entities are not supported".into())
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if stack.len() != 1 {
        return Err("Unclosed XML element".into());
    }
    let mut root = stack.pop().unwrap();
    if root.children.len() != 1 || !root.text.trim().is_empty() {
        return Err("Office XML requires one root element".into());
    }
    Ok(root.children.remove(0))
}

fn package(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| "Invalid Office ZIP package")?;
    if archive.len() > MAX_ZIP_ENTRIES {
        return Err(limit("ZIP exceeds 4096 entries"));
    }
    let mut seen = HashSet::new();
    let mut total = 0usize;
    let mut out = BTreeMap::new();
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|_| "Cannot read Office ZIP entry (possibly encrypted)")?;
        let name = file.name().to_owned();
        if name.contains('\\')
            || name.contains('\0')
            || name.starts_with('/')
            || Path::new(&name)
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
            || file.is_symlink()
        {
            return Err("Office ZIP contains an unsafe entry path".into());
        }
        if !seen.insert(name.clone()) {
            return Err("Office ZIP contains duplicate entry names".into());
        }
        if file.encrypted() {
            return Err("Encrypted Office ZIP packages cannot be read".into());
        }
        if file.is_dir() {
            continue;
        }
        if file.size() > MAX_ENTRY_BYTES as u64 {
            return Err(limit("ZIP entry exceeds 16 MiB"));
        }
        let remaining = MAX_ZIP_BYTES
            .checked_sub(total)
            .ok_or_else(|| limit("ZIP expansion exceeds 64 MiB"))?;
        if file.size() > remaining as u64 {
            return Err(limit("ZIP expansion exceeds 64 MiB"));
        }
        let cap = remaining.min(MAX_ENTRY_BYTES);
        let mut data = Vec::new();
        (&mut file)
            .take(cap as u64 + 1)
            .read_to_end(&mut data)
            .map_err(|_| "Office ZIP decompression or checksum verification failed")?;
        if data.len() > cap {
            return Err(limit("ZIP decompression exceeds its byte budget"));
        }
        total += data.len();
        if data.len() as u64 != file.size() {
            return Err("Office ZIP entry size mismatch".into());
        }
        if name.ends_with(".xml") || name.ends_with(".rels") {
            out.insert(name, data);
        }
    }
    Ok(out)
}
fn part(package: &BTreeMap<String, Vec<u8>>, name: &str) -> Result<Node> {
    xml(package
        .get(name)
        .ok_or_else(|| format!("Missing Office part: {name}"))?)
}
fn append(out: &mut String, text: &str) -> Result<()> {
    if out.len() + text.len() > MAX_TEXT_BYTES {
        return Err(limit("extracted text exceeds 8 MiB"));
    }
    out.push_str(text);
    Ok(())
}
fn paragraph_text(node: &Node) -> String {
    if node.name == "t" {
        return node.value();
    }
    if node.name == "tab" {
        return "\t".into();
    }
    if node.name == "br" || node.name == "cr" {
        return "\n".into();
    }
    node.children.iter().map(paragraph_text).collect()
}
fn paragraphs(root: &Node, out: &mut String) -> Result<()> {
    if root.name == "p" {
        let value = paragraph_text(root);
        append(out, &value)?;
        append(out, "\n")?;
    } else {
        for c in &root.children {
            paragraphs(c, out)?;
        }
    }
    Ok(())
}
fn rel_target(base: &str, target: &str) -> Result<String> {
    if target.contains('\\')
        || target.contains('\0')
        || target.contains(':')
        || target.contains('?')
        || target.contains('#')
    {
        return Err("Unsupported Office relationship target".into());
    }
    let mut parts: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        base.split('/').filter(|s| !s.is_empty()).collect()
    };
    for part in target.trim_start_matches('/').split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err("Office relationship escapes package".into());
                }
            }
            other => parts.push(other),
        }
    }
    Ok(parts.join("/"))
}
fn relationships(
    package: &BTreeMap<String, Vec<u8>>,
    path: &str,
    base: &str,
) -> Result<BTreeMap<String, String>> {
    let root = part(package, path)?;
    let mut out = BTreeMap::new();
    for rel in root.all("Relationship") {
        if rel.attr("TargetMode") == Some("External") {
            continue;
        }
        let id = rel.attr("Id").ok_or("Office relationship lacks Id")?;
        let target = rel
            .attr("Target")
            .ok_or("Office relationship lacks Target")?;
        if out.insert(id.into(), rel_target(base, target)?).is_some() {
            return Err("Duplicate Office relationship Id".into());
        }
    }
    Ok(out)
}
fn parse_office(kind: &str, bytes: &[u8]) -> Result<Parsed> {
    let package = package(bytes)?;
    let content_types = part(&package, "[Content_Types].xml")?;
    if content_types.name != "Types" {
        return Err("Invalid Office content types".into());
    }
    let mut parsed=Parsed{warnings:vec!["Only document text is extracted; embedded objects, macros and external links are not executed.".into()],..Default::default()};
    let mut text = String::new();
    let mut has_text = false;
    match kind {
        "docx" => {
            let document = part(&package, "word/document.xml")?;
            if document.name != "document" {
                return Err("Invalid Word document root".into());
            }
            has_text |= document
                .all("t")
                .iter()
                .any(|n| !n.value().trim().is_empty());
            paragraphs(&document, &mut text)?;
            for (name, bytes) in &package {
                if name.starts_with("word/")
                    && name.ends_with(".xml")
                    && (name.starts_with("word/header")
                        || name.starts_with("word/footer")
                        || matches!(
                            name.as_str(),
                            "word/footnotes.xml" | "word/endnotes.xml" | "word/comments.xml"
                        ))
                {
                    append(&mut text, &format!("\n[{name}]\n"))?;
                    let extra = xml(bytes)?;
                    has_text |= extra.all("t").iter().any(|n| !n.value().trim().is_empty());
                    paragraphs(&extra, &mut text)?;
                }
            }
        }
        "pptx" => {
            let presentation = part(&package, "ppt/presentation.xml")?;
            if presentation.name != "presentation" {
                return Err("Invalid PowerPoint presentation root".into());
            }
            let rels = relationships(&package, "ppt/_rels/presentation.xml.rels", "ppt")?;
            let slides = presentation.all("sldId");
            parsed.page_count = Some(slides.len());
            for (i, slide) in slides.iter().enumerate() {
                let rid = slide
                    .relationship_id()
                    .ok_or("Slide lacks relationship id")?;
                let target = rels.get(rid).ok_or("Slide relationship not found")?;
                append(&mut text, &format!("[Slide {}]\n", i + 1))?;
                let slide = part(&package, target)?;
                if slide.name != "sld" {
                    return Err("Invalid PowerPoint slide root".into());
                }
                has_text |= slide.all("t").iter().any(|n| !n.value().trim().is_empty());
                paragraphs(&slide, &mut text)?;
                if let Some((base, filename)) = target.rsplit_once('/') {
                    let rel_path = format!("{base}/_rels/{filename}.rels");
                    if package.contains_key(&rel_path) {
                        for notes in relationships(&package, &rel_path, base)?
                            .values()
                            .filter(|n| n.starts_with("ppt/notesSlides/") && n.ends_with(".xml"))
                        {
                            let notes = part(&package, notes)?;
                            if notes.name != "notes" {
                                return Err("Invalid PowerPoint notes root".into());
                            }
                            has_text |= notes.all("t").iter().any(|n| !n.value().trim().is_empty());
                            append(&mut text, "[Speaker notes]\n")?;
                            paragraphs(&notes, &mut text)?;
                        }
                    }
                }
            }
        }
        "xlsx" => {
            let workbook = part(&package, "xl/workbook.xml")?;
            if workbook.name != "workbook" {
                return Err("Invalid Excel workbook root".into());
            }
            let rels = relationships(&package, "xl/_rels/workbook.xml.rels", "xl")?;
            let shared = if let Some(bytes) = package.get("xl/sharedStrings.xml") {
                xml(bytes)?
                    .all("si")
                    .into_iter()
                    .map(|n| n.all("t").into_iter().map(Node::value).collect::<String>())
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let sheets = workbook.all("sheet");
            parsed.sheet_count = Some(sheets.len());
            for sheet in sheets {
                let name = sheet.attr("name").ok_or("Sheet lacks name")?;
                let target = rels
                    .get(
                        sheet
                            .relationship_id()
                            .ok_or("Sheet lacks relationship id")?,
                    )
                    .ok_or("Sheet relationship not found")?;
                append(
                    &mut text,
                    &format!("[Sheet {}]\n", serde_json::to_string(name).unwrap()),
                )?;
                let worksheet = part(&package, target)?;
                if worksheet.name != "worksheet" {
                    return Err("Invalid Excel worksheet root".into());
                }
                for cell in worksheet.all("c") {
                    let address = cell.attr("r").unwrap_or("?");
                    let ty = cell.attr("t").unwrap_or("n");
                    let raw = cell
                        .children
                        .iter()
                        .find(|n| n.name == "v")
                        .map(Node::value)
                        .unwrap_or_default();
                    let value = match ty {
                        "s" => shared
                            .get(
                                raw.parse::<usize>()
                                    .map_err(|_| "Invalid shared string index")?,
                            )
                            .ok_or("Shared string index out of bounds")?
                            .clone(),
                        "inlineStr" => cell.all("t").iter().map(|n| n.value()).collect(),
                        "b" => match raw.as_str() {
                            "0" => "false".into(),
                            "1" => "true".into(),
                            _ => return Err("Invalid boolean cell".into()),
                        },
                        _ => raw,
                    };
                    let formula = cell.children.iter().find(|n| n.name == "f");
                    if let Some(formula) = formula {
                        has_text = true;
                        let f = formula.value();
                        let mut descriptor =
                            format!("{address}\tformula={}", serde_json::to_string(&f).unwrap());
                        if !formula.attrs.is_empty() {
                            descriptor.push_str(&format!(
                                " attributes={}",
                                serde_json::to_string(&formula.attrs).unwrap()
                            ));
                        }
                        descriptor.push_str(&format!(
                            "\tcachedValue={}\n",
                            serde_json::to_string(&value).unwrap()
                        ));
                        append(&mut text, &descriptor)?;
                    } else if !value.is_empty() {
                        has_text |= !value.trim().is_empty();
                        append(
                            &mut text,
                            &format!("{address}\t{}\n", serde_json::to_string(&value).unwrap()),
                        )?;
                    }
                }
            }
            parsed.warnings.push("Spreadsheet formulas are preserved with cached values; formulas and external data are not recalculated. Numeric dates remain stored numbers.".into());
        }
        _ => unreachable!(),
    }
    parsed.text = Some(text);
    parsed.has_document_text = Some(has_text);
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        root: PathBuf,
        store: Store,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "mcodex-attachments-{:032x}",
                rand::random::<u128>()
            ));
            private_dir(&root).unwrap();
            Self {
                store: Store {
                    root: root.join("files"),
                },
                root,
            }
        }
        fn import(&self, name: &str, data: &[u8]) -> Value {
            self.store
                .rpc(
                    "files/import",
                    &json!({"name":name,"dataBase64":STANDARD.encode(data)}),
                )
                .unwrap()["attachment"]
                .clone()
        }
        fn text(&self, id: &str) -> String {
            self.store
                .rpc("files/read", &json!({"id":id,"includeContent":true}))
                .unwrap()["content"]["text"]
                .as_str()
                .unwrap()
                .into()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    fn office(parts: &[(&str, &str)]) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("[Content_Types].xml", options).unwrap();
        writer
            .write_all(
                b"<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>",
            )
            .unwrap();
        for (name, content) in parts {
            writer.start_file(*name, options).unwrap();
            writer.write_all(content.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }
    fn pdf(text: Option<&str>) -> Vec<u8> {
        use lopdf::{
            content::{Content, Operation},
            dictionary, Object, Stream,
        };
        let mut doc = lopdf::Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id=doc.add_object(dictionary!{"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica","Encoding"=>"WinAnsiEncoding"});
        let resources_id = doc.add_object(dictionary! {"Font"=>dictionary!{"F1"=>font_id}});
        let operations = if let Some(text) = text {
            vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Td", vec![20.into(), 40.into()]),
                Operation::new("Tj", vec![Object::string_literal(text)]),
                Operation::new("ET", vec![]),
            ]
        } else {
            Vec::new()
        };
        let content = Content { operations }.encode().unwrap();
        let content_id = doc.add_object(Stream::new(dictionary! {}, content));
        let page_id =
            doc.add_object(dictionary! {"Type"=>"Page","Parent"=>pages_id,"Contents"=>content_id});
        doc.objects.insert(pages_id,dictionary!{"Type"=>"Pages","Kids"=>vec![page_id.into()],"Count"=>1,"Resources"=>resources_id,"MediaBox"=>vec![0.into(),0.into(),600.into(),800.into()]}.into());
        let catalog_id = doc.add_object(dictionary! {"Type"=>"Catalog","Pages"=>pages_id});
        doc.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn text_csv_json_markdown_and_utf16_survive_restart_without_content_loss() {
        let f = Fixture::new();
        let input = "中文 😀\r\nname,value\r\n\"two, fields\",=SUM(A1:A2)\r\n";
        for name in ["table.csv", "notes.md", "data.json", "script.html"] {
            let m = f.import(name, input.as_bytes());
            assert_eq!(m["parseStatus"], "ready");
            assert_eq!(f.text(m["id"].as_str().unwrap()), input);
        }
        let mut utf16 = vec![0xff, 0xfe];
        for c in "UTF16 中文".encode_utf16() {
            utf16.extend(c.to_le_bytes());
        }
        let m = f.import("wide.txt", &utf16);
        let restarted = Store {
            root: f.store.root.clone(),
        };
        assert_eq!(
            restarted
                .rpc("files/read", &json!({"id":m["id"],"includeContent":true}))
                .unwrap()["content"]["text"],
            "UTF16 中文"
        );
        let public = serde_json::to_string(&m).unwrap();
        assert!(!public.contains(f.root.to_str().unwrap()));
        assert!(m.get("path").is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = f.store.root.join(m["id"].as_str().unwrap());
            assert_eq!(
                fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
            for name in ["original.bin", "metadata.json", "text.txt"] {
                assert_eq!(
                    fs::metadata(dir.join(name)).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }
    }

    #[test]
    fn docx_extracts_entities_unicode_paragraphs_and_headers_without_executing_markup() {
        let f = Fixture::new();
        let bytes=office(&[("word/document.xml","<w:document xmlns:w=\"w\"><w:body><w:p><w:r><w:t>中文 &amp; &#x1F600;</w:t><w:tab/><w:t>&lt;script&gt;literal&lt;/script&gt;</w:t></w:r></w:p><w:p><w:r><w:t>second</w:t></w:r></w:p></w:body></w:document>"),("word/header1.xml","<w:hdr xmlns:w=\"w\"><w:p><w:r><w:t>Header</w:t></w:r></w:p></w:hdr>")]);
        let m = f.import("test.docx", &bytes);
        assert_eq!(m["parseStatus"], "ready");
        let text = f.text(m["id"].as_str().unwrap());
        let export = f
            .store
            .rpc(
                "files/read",
                &json!({"id":m["id"],"includeContent":true,"includeOriginal":true}),
            )
            .unwrap();
        assert_eq!(export["content"]["dataBase64"], STANDARD.encode(&bytes));
        assert_eq!(export["content"]["text"], text);
        assert!(f
            .store
            .rpc("files/read", &json!({"id":m["id"]}))
            .unwrap()
            .get("content")
            .is_none());
        assert!(text.contains("中文 & 😀\t<script>literal</script>\nsecond\n"));
        assert!(text.contains("Header"));
        let invalid = office(&[(
            "word/document.xml",
            "<!DOCTYPE doc [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><doc><p><t>&x;</t></p></doc>",
        )]);
        let m = f.import("unsafe.docx", &invalid);
        assert_eq!(m["parseStatus"], "failed");
        assert!(m["error"].as_str().unwrap().contains("external entities"));
        assert!(f
            .store
            .with_lock(|s| s.model_content(m["id"].as_str().unwrap()))
            .is_err());
    }

    #[test]
    fn pptx_uses_relationship_order_and_xlsx_preserves_values_formulas_and_cache() {
        let f = Fixture::new();
        let ppt=office(&[("ppt/presentation.xml","<p:presentation xmlns:p=\"p\" xmlns:r=\"r\"><p:sldIdLst><p:sldId id=\"20\" r:id=\"second\"/><p:sldId id=\"10\" r:id=\"first\"/></p:sldIdLst></p:presentation>"),("ppt/_rels/presentation.xml.rels","<Relationships><Relationship Id=\"first\" Target=\"slides/slide1.xml\"/><Relationship Id=\"second\" Target=\"slides/slide2.xml\"/></Relationships>"),("ppt/slides/slide1.xml","<sld><p><t>FIRST</t></p></sld>"),("ppt/slides/slide2.xml","<sld><p><t>SECOND</t></p></sld>")]);
        let m = f.import("deck.pptx", &ppt);
        assert_eq!(m["pageCount"], 2);
        let text = f.text(m["id"].as_str().unwrap());
        assert!(text.find("SECOND").unwrap() < text.find("FIRST").unwrap());
        let empty=office(&[("ppt/presentation.xml","<presentation xmlns:r=\"r\"><sldId r:id=\"r1\"/></presentation>"),("ppt/_rels/presentation.xml.rels","<Relationships><Relationship Id=\"r1\" Target=\"slides/slide1.xml\"/></Relationships>"),("ppt/slides/slide1.xml","<sld><p><t> </t></p></sld>")]);
        assert_eq!(f.import("blank.pptx", &empty)["parseStatus"], "no_text");
        let xlsx=office(&[("xl/workbook.xml","<workbook xmlns:r=\"r\"><sheets><sheet name=\"账目 &amp; data\" r:id=\"r1\"/></sheets></workbook>"),("xl/_rels/workbook.xml.rels","<Relationships><Relationship Id=\"r1\" Target=\"worksheets/sheet1.xml\"/></Relationships>"),("xl/sharedStrings.xml","<sst><si><r><t>shared </t></r><r><t>中文</t></r></si></sst>"),("xl/worksheets/sheet1.xml","<worksheet><sheetData><row r=\"1\"><c r=\"A1\" t=\"s\"><v>0</v></c><c r=\"B1\" t=\"inlineStr\"><is><t>inline &amp; value</t></is></c><c r=\"C1\" t=\"b\"><v>1</v></c><c r=\"D1\"><f>SUM(A2:A3)</f><v>42.50</v></c><c r=\"E1\"><f t=\"shared\" si=\"0\"/><v>3</v></c></row></sheetData></worksheet>")]);
        let m = f.import("book.xlsx", &xlsx);
        assert_eq!(m["parseStatus"], "ready");
        assert_eq!(m["sheetCount"], 1);
        let text = f.text(m["id"].as_str().unwrap());
        for value in [
            "账目 & data",
            "shared 中文",
            "inline & value",
            "true",
            "SUM(A2:A3)",
            "42.50",
            "attributes=",
            "cachedValue=",
        ] {
            assert!(text.contains(value), "missing {value} in {text}");
        }
    }

    #[test]
    fn pdf_distinguishes_real_text_empty_pages_and_malformed_documents() {
        let f = Fixture::new();
        let m = f.import("text.pdf", &pdf(Some("Readable PDF 42")));
        assert_eq!(m["parseStatus"], "ready", "{m}");
        assert_eq!(m["pageCount"], 1);
        assert!(f
            .text(m["id"].as_str().unwrap())
            .contains("Readable PDF 42"));
        let empty = f.import("scan.pdf", &pdf(None));
        assert_eq!(empty["parseStatus"], "no_text");
        assert!(empty["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains("OCR")));
        assert!(f
            .store
            .with_lock(|s| s.model_content(empty["id"].as_str().unwrap()))
            .is_err());
        let malformed = f.import("broken.pdf", b"%PDF-1.5\nnot a document");
        assert_eq!(malformed["parseStatus"], "failed");
        assert!(malformed["error"].as_str().unwrap().contains("PDF"));
        let mut bomb = lopdf::Document::load_mem(&pdf(None)).unwrap();
        for object in bomb.objects.values_mut() {
            if let lopdf::Object::Stream(stream) = object {
                stream.set_plain_content(vec![b' '; MAX_ENTRY_BYTES + 1]);
            }
        }
        bomb.compress();
        let mut bytes = Vec::new();
        bomb.save_to(&mut bytes).unwrap();
        assert!(bytes.len() < MAX_FILE_BYTES);
        assert!(f
            .store
            .rpc(
                "files/import",
                &json!({"name":"oversized-stream.pdf","dataBase64":STANDARD.encode(bytes)})
            )
            .unwrap_err()
            .contains("PDF stream decompression"));
    }

    #[test]
    fn image_original_is_validated_and_returned_losslessly_to_the_model() {
        let f = Fixture::new();
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2,
            3,
            image::Rgba([12, 34, 56, 255]),
        ));
        let mut cursor = Cursor::new(Vec::new());
        img.write_to(&mut cursor, image::ImageFormat::Png).unwrap();
        let bytes = cursor.into_inner();
        let generated = f
            .store
            .with_lock(|s| s.import_image("generated.png", "image/png", &bytes))
            .unwrap();
        let (metadata, original) = f
            .store
            .with_lock(|s| s.image_original(generated["id"].as_str().unwrap()))
            .unwrap();
        assert_eq!(original, bytes);
        assert_eq!(metadata["width"], 2);
        assert!(f
            .store
            .with_lock(|s| s.import_image("bad.png", "image/png", b"not an image"))
            .is_err());
        let m = f.import("image.png", &bytes);
        assert_eq!(m["parseStatus"], "ready");
        assert_eq!(m["width"], 2);
        assert_eq!(m["height"], 3);
        let content = f
            .store
            .with_lock(|s| s.model_content(m["id"].as_str().unwrap()))
            .unwrap();
        assert_eq!(
            content[0]["image_url"],
            format!("data:image/png;base64,{}", STANDARD.encode(&bytes))
        );
        let read = f
            .store
            .rpc("files/read", &json!({"id":m["id"],"includeContent":true}))
            .unwrap();
        assert_eq!(read["content"]["dataBase64"], STANDARD.encode(bytes));
        assert!(f
            .store
            .rpc(
                "files/import",
                &json!({"name":"fake.png","dataBase64":STANDARD.encode(b"text")})
            )
            .is_err());
    }

    #[test]
    fn zip_and_text_limits_reject_without_committing_partial_attachments() {
        let f = Fixture::new();
        let text = "x".repeat(MAX_TEXT_BYTES + 1);
        assert!(f
            .store
            .rpc(
                "files/import",
                &json!({"name":"big.txt","dataBase64":STANDARD.encode(text.as_bytes())})
            )
            .unwrap_err()
            .contains("limit exceeded"));
        let huge = "x".repeat(MAX_ENTRY_BYTES + 1);
        let bytes = office(&[("word/document.xml", &huge)]);
        assert!(bytes.len() < MAX_FILE_BYTES);
        assert!(f
            .store
            .rpc(
                "files/import",
                &json!({"name":"bomb.docx","dataBase64":STANDARD.encode(bytes)})
            )
            .unwrap_err()
            .contains("ZIP entry"));
        let repeated = "x".repeat(14 * 1024 * 1024);
        let bytes = office(&[
            ("word/a.xml", &repeated),
            ("word/b.xml", &repeated),
            ("word/c.xml", &repeated),
            ("word/d.xml", &repeated),
            ("word/e.xml", &repeated),
        ]);
        assert!(f
            .store
            .rpc(
                "files/import",
                &json!({"name":"total.docx","dataBase64":STANDARD.encode(bytes)})
            )
            .unwrap_err()
            .contains("ZIP expansion"));
        assert_eq!(
            f.store.rpc("files/list", &json!({})).unwrap()["data"],
            json!([])
        );
        assert!(fs::read_dir(&f.store.root).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".import-")));
    }

    #[test]
    fn imports_are_unique_concurrent_and_delete_is_persistent_and_scoped() {
        let f = Fixture::new();
        let mut threads = Vec::new();
        for _ in 0..20 {
            let store = f.store.clone();
            threads.push(std::thread::spawn(move || {
                store
                    .rpc(
                        "files/import",
                        &json!({"name":"same.txt","dataBase64":STANDARD.encode("same bytes")}),
                    )
                    .unwrap()["attachment"]["id"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            }));
        }
        let ids: HashSet<String> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(ids.len(), 20);
        assert_eq!(
            f.store.rpc("files/list", &json!({})).unwrap()["data"]
                .as_array()
                .unwrap()
                .len(),
            20
        );
        let id = ids.iter().next().unwrap();
        f.store.rpc("files/delete", &json!({"id":id})).unwrap();
        assert!(Store {
            root: f.store.root.clone()
        }
        .rpc("files/read", &json!({"id":id}))
        .is_err());
        assert!(f
            .store
            .rpc("files/delete", &json!({"id":"../../outside"}))
            .is_err());
        assert_eq!(
            f.store.rpc("files/list", &json!({})).unwrap()["data"]
                .as_array()
                .unwrap()
                .len(),
            19
        );
    }

    #[test]
    fn corruption_unsafe_zip_paths_and_storage_symlinks_do_not_reach_model() {
        let f = Fixture::new();
        let m = f.import("notes.txt", b"safe text");
        let id = m["id"].as_str().unwrap();
        let path = f.store.root.join(id).join("text.txt");
        fs::write(&path, b"modified").unwrap();
        assert!(f
            .store
            .with_lock(|s| s.model_content(id))
            .unwrap_err()
            .contains("integrity"));
        let bytes = office(&[("../word/document.xml", "<doc/>")]);
        let m = f.import("unsafe.docx", &bytes);
        assert_eq!(m["parseStatus"], "failed");
        assert!(m["error"].as_str().unwrap().contains("unsafe entry"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            fs::remove_file(&path).unwrap();
            let outside = f.root.join("outside");
            fs::write(&outside, b"secret").unwrap();
            symlink(&outside, &path).unwrap();
            assert!(f
                .store
                .with_lock(|s| s.model_content(id))
                .unwrap_err()
                .contains("unsafe file"));
            assert!(f.store.rpc("files/delete", &json!({"id":id})).is_err());
            assert_eq!(fs::read(outside).unwrap(), b"secret");
        }
    }
}
