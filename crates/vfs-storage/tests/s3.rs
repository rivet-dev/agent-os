use agentos_vfs_core::engine::engines::{ChunkedFs, ChunkedFsOptions, ObjectFs};
use agentos_vfs_core::engine::{
    BlockKey, BlockStore, InodeType, VirtualFileSystem, S_IFBLK, S_IFCHR, S_IFIFO,
};
use agentos_vfs_storage::{
    S3BlockStore, S3BlockStoreOptions, S3ObjectBackend, S3ObjectBackendOptions,
};
use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::Builder as S3ConfigBuilder;
use aws_sdk_s3::Client;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[tokio::test]
async fn s3_block_store_round_trips_and_cleans_blocks() {
    let server = MockS3Server::start();
    let store = S3BlockStore::with_options(
        s3_client(server.base_url()).await,
        "test-bucket",
        S3BlockStoreOptions {
            prefix: "blocks/".to_string(),
        },
    );
    let first = BlockKey::from_content(b"abcdef");
    let second = BlockKey::from_content(b"ghijkl");

    store.put(&first, b"abcdef").await.unwrap();
    assert!(store.exists(&first).await.unwrap());
    assert_eq!(store.get_range(&first, 2, 3).await.unwrap(), b"cde");
    store.copy(&first, &second).await.unwrap();
    assert_eq!(store.get(&second).await.unwrap(), b"abcdef");

    store
        .delete_many(&[first.clone(), second.clone()])
        .await
        .unwrap();
    assert!(!store.exists(&first).await.unwrap());
    assert!(!store.exists(&second).await.unwrap());
    assert!(server
        .requests()
        .iter()
        .any(|request| request.method == "POST"));
}

#[tokio::test]
async fn object_s3_round_trips_native_objects() {
    let server = MockS3Server::start();
    let backend = S3ObjectBackend::with_options(
        s3_client(server.base_url()).await,
        "test-bucket",
        S3ObjectBackendOptions {
            prefix: "objects/".to_string(),
        },
    );
    let fs = ObjectFs::new(backend);

    fs.write_file("/dir/file.txt", b"hello object s3")
        .await
        .unwrap();
    let before_atime = fs.stat("/dir/file.txt").await.unwrap();
    let atime_ms = u64::try_from(before_atime.atime.sec).unwrap() * 1_000
        + u64::from(before_atime.atime.nsec / 1_000_000)
        + 2_000;
    fs.set_atime("/dir/file.txt", atime_ms).await.unwrap();
    let after_atime = fs.stat("/dir/file.txt").await.unwrap();
    assert!(after_atime.atime > before_atime.atime);
    assert_eq!(after_atime.mtime, before_atime.mtime);
    assert_eq!(after_atime.ctime, before_atime.ctime);
    assert_eq!(after_atime.birthtime, before_atime.birthtime);
    assert_eq!(fs.realpath("/dir/file.txt").await.unwrap(), "/dir/file.txt");
    assert_eq!(
        fs.read_file("/dir/file.txt").await.unwrap(),
        b"hello object s3"
    );
    assert_eq!(fs.pread("/dir/file.txt", 6, 6).await.unwrap(), b"object");
    assert!(fs.exists("/dir/file.txt").await);
    assert_eq!(fs.read_dir("/dir").await.unwrap(), vec!["file.txt"]);

    let requests_before_invalid_xattr = server.requests().len();
    let invalid_name = format!("user.{}", "x".repeat(256));
    assert_eq!(
        fs.set_xattr("/dir/file.txt", &invalid_name, b"value", 0, true)
            .await
            .unwrap_err()
            .code(),
        "ERANGE"
    );
    assert_eq!(server.requests().len(), requests_before_invalid_xattr);

    fs.rename("/dir/file.txt", "/dir/renamed.txt")
        .await
        .unwrap();
    assert_eq!(
        fs.read_file("/dir/renamed.txt").await.unwrap(),
        b"hello object s3"
    );
    assert!(!fs.exists("/dir/file.txt").await);
    assert!(server
        .object_keys()
        .iter()
        .any(|key| key == "test-bucket/objects/dir/renamed.txt"));
}

#[tokio::test]
async fn object_s3_resolves_explicit_directory_markers_for_new_files() {
    let server = MockS3Server::start();
    let backend = S3ObjectBackend::with_options(
        s3_client(server.base_url()).await,
        "test-bucket",
        S3ObjectBackendOptions {
            prefix: "objects/".to_string(),
        },
    );
    let fs = ObjectFs::new(backend);

    fs.mkdir("/fill-probe", false).await.unwrap();
    assert_eq!(fs.realpath("/fill-probe").await.unwrap(), "/fill-probe");
    assert!(fs.stat("/fill-probe").await.unwrap().is_directory);
    fs.chmod("/fill-probe", 0o700).await.unwrap();
    fs.set_xattr("/fill-probe", "user.marker", b"directory", 0, true)
        .await
        .unwrap();
    assert_eq!(fs.stat("/fill-probe").await.unwrap().mode & 0o7777, 0o700);
    assert_eq!(
        fs.get_xattr("/fill-probe", "user.marker", true)
            .await
            .unwrap(),
        b"directory"
    );
    fs.write_file("/fill-probe/small", b"").await.unwrap();
    fs.chmod("/fill-probe/small", 0o600).await.unwrap();
    assert_eq!(
        fs.stat("/fill-probe/small").await.unwrap().mode & 0o7777,
        0o600
    );
    assert_eq!(
        fs.realpath("/fill-probe/small").await.unwrap(),
        "/fill-probe/small"
    );
    fs.truncate("/fill-probe/small", 0).await.unwrap();
}

#[tokio::test]
async fn object_s3_preserves_and_cleans_special_nodes_under_directory_markers() {
    let server = MockS3Server::start();
    let backend = S3ObjectBackend::with_options(
        s3_client(server.base_url()).await,
        "test-bucket",
        S3ObjectBackendOptions {
            prefix: "objects/".to_string(),
        },
    );
    let fs = ObjectFs::new(backend);

    fs.mkdir("/dev", false).await.unwrap();
    fs.mknod("/dev/block", S_IFBLK | 0o640, (8 << 8) | 1)
        .await
        .unwrap();
    fs.mknod("/dev/char", S_IFCHR | 0o600, (1 << 8) | 3)
        .await
        .unwrap();
    fs.mknod("/dev/fifo", S_IFIFO | 0o600, 0).await.unwrap();

    assert_eq!(fs.realpath("/dev").await.unwrap(), "/dev");
    let entries = fs.read_dir_with_types("/dev").await.unwrap();
    assert!(entries
        .iter()
        .any(|entry| entry.name == "block" && entry.kind == InodeType::BlockDevice));
    assert!(entries
        .iter()
        .any(|entry| entry.name == "char" && entry.kind == InodeType::CharacterDevice));
    assert!(entries
        .iter()
        .any(|entry| entry.name == "fifo" && entry.kind == InodeType::Fifo));
    assert_eq!(fs.stat("/dev/block").await.unwrap().rdev, (8 << 8) | 1);

    fs.remove_file("/dev/block").await.unwrap();
    fs.remove_file("/dev/char").await.unwrap();
    fs.remove_file("/dev/fifo").await.unwrap();
    fs.remove_dir("/dev").await.unwrap();
    assert!(!fs.exists("/dev").await);
}

#[tokio::test]
async fn chunked_s3_reopens_metadata_and_cleans_truncated_chunks() {
    let server = MockS3Server::start();
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("metadata.sqlite");
    let stale_key = BlockKey::from_content(b"efgh");

    {
        let metadata = agentos_vfs_storage::SqliteMetadataStore::open(&db).unwrap();
        let blocks = S3BlockStore::with_options(
            s3_client(server.base_url()).await,
            "test-bucket",
            S3BlockStoreOptions {
                prefix: "chunked/blocks/".to_string(),
            },
        );
        let fs = ChunkedFs::with_options(
            metadata,
            blocks,
            ChunkedFsOptions {
                inline_threshold: 1,
                chunk_size: 4,
                ..ChunkedFsOptions::default()
            },
        );
        fs.write_file("/file", b"abcdefgh").await.unwrap();
    }

    let metadata = agentos_vfs_storage::SqliteMetadataStore::open(&db).unwrap();
    let blocks = S3BlockStore::with_options(
        s3_client(server.base_url()).await,
        "test-bucket",
        S3BlockStoreOptions {
            prefix: "chunked/blocks/".to_string(),
        },
    );
    let fs = ChunkedFs::with_options(
        metadata,
        blocks,
        ChunkedFsOptions {
            inline_threshold: 1,
            chunk_size: 4,
            ..ChunkedFsOptions::default()
        },
    );

    assert_eq!(fs.read_file("/file").await.unwrap(), b"abcdefgh");
    fs.truncate("/file", 5).await.unwrap();
    assert_eq!(fs.read_file("/file").await.unwrap(), b"abcde");
    assert!(!server
        .object_keys()
        .iter()
        .any(|key| { key == &format!("test-bucket/chunked/blocks/{}", stale_key.0) }));
}

async fn s3_client(endpoint: &str) -> Client {
    let shared_config = aws_config::defaults(BehaviorVersion::latest())
        .region(aws_sdk_s3::config::Region::new("us-east-1"))
        .credentials_provider(Credentials::new(
            "minioadmin",
            "minioadmin",
            None,
            None,
            "agentos-vfs-test",
        ))
        .load()
        .await;
    Client::from_conf(
        S3ConfigBuilder::from(&shared_config)
            .endpoint_url(endpoint)
            .force_path_style(true)
            .build(),
    )
}

#[derive(Clone, Debug)]
struct LoggedRequest {
    method: String,
}

#[derive(Clone, Debug, Default)]
struct StoredObject {
    body: Vec<u8>,
    metadata: BTreeMap<String, String>,
}

struct MockS3Server {
    base_url: String,
    shutdown: Arc<AtomicBool>,
    objects: Arc<Mutex<BTreeMap<String, StoredObject>>>,
    requests: Arc<Mutex<Vec<LoggedRequest>>>,
    handle: Option<JoinHandle<()>>,
}

impl MockS3Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock s3");
        listener
            .set_nonblocking(true)
            .expect("configure mock s3 listener");
        let address = listener.local_addr().expect("resolve mock s3 address");
        let shutdown = Arc::new(AtomicBool::new(false));
        let objects = Arc::new(Mutex::new(BTreeMap::new()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let shutdown_for_thread = Arc::clone(&shutdown);
        let objects_for_thread = Arc::clone(&objects);
        let requests_for_thread = Arc::clone(&requests);

        let handle = thread::spawn(move || {
            while !shutdown_for_thread.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        handle_stream(stream, &objects_for_thread, &requests_for_thread)
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            base_url: format!("http://{}", address),
            shutdown,
            objects,
            requests,
            handle: Some(handle),
        }
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn object_keys(&self) -> Vec<String> {
        self.objects
            .lock()
            .expect("lock mock s3 objects")
            .keys()
            .cloned()
            .collect()
    }

    fn requests(&self) -> Vec<LoggedRequest> {
        self.requests.lock().expect("lock mock s3 requests").clone()
    }
}

impl Drop for MockS3Server {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            handle.join().expect("join mock s3 thread");
        }
    }
}

fn handle_stream(
    mut stream: TcpStream,
    objects: &Arc<Mutex<BTreeMap<String, StoredObject>>>,
    requests: &Arc<Mutex<Vec<LoggedRequest>>>,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set mock s3 read timeout");

    let mut buffer = Vec::new();
    let mut header_end = None;
    while header_end.is_none() {
        let mut chunk = [0; 1024];
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(read) => {
                buffer.extend_from_slice(&chunk[..read]);
                header_end = find_header_end(&buffer);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(_) => return,
        }
    }

    let header_end = header_end.expect("parse mock s3 headers");
    let header_text = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = header_text.split("\r\n");
    let request_line = match lines.next() {
        Some(line) if !line.is_empty() => line,
        _ => return,
    };
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or_default().to_owned();
    let raw_target = request_parts.next().unwrap_or_default();
    let (raw_path, raw_query) = raw_target.split_once('?').unwrap_or((raw_target, ""));
    let raw_query = raw_query.to_owned();
    let path = decode_url_component(raw_path);

    let mut headers = BTreeMap::new();
    let mut content_length = 0usize;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim().to_owned();
            if name == "content-length" {
                content_length = value.parse::<usize>().unwrap_or(0);
            }
            headers.insert(name, value);
        }
    }

    while buffer.len() < header_end + 4 + content_length {
        let mut chunk = [0; 1024];
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(_) => break,
        }
    }
    let body = buffer[header_end + 4..header_end + 4 + content_length].to_vec();
    requests
        .lock()
        .expect("lock mock s3 request log")
        .push(LoggedRequest {
            method: method.clone(),
        });

    match method.as_str() {
        "GET" if raw_query.contains("list-type=2") => {
            let query = parse_query(&raw_query);
            let xml = list_objects_xml(
                &path,
                query.get("prefix").map(String::as_str).unwrap_or_default(),
                query.get("delimiter").map(String::as_str),
                objects,
            );
            send_response(&mut stream, 200, "OK", "application/xml", xml.as_bytes());
        }
        "GET" => {
            let key = path.trim_start_matches('/');
            if let Some(object) = objects
                .lock()
                .expect("lock mock s3 objects")
                .get(key)
                .cloned()
            {
                let bytes = apply_range(&object.body, headers.get("range").map(String::as_str));
                send_response(&mut stream, 200, "OK", "application/octet-stream", &bytes);
            } else {
                send_not_found(&mut stream);
            }
        }
        "HEAD" => {
            let key = path.trim_start_matches('/');
            let object = objects
                .lock()
                .expect("lock mock s3 objects")
                .get(key)
                .cloned();
            match object {
                Some(object) => send_head_response(&mut stream, 200, "OK", &object),
                None => send_head_response(&mut stream, 404, "Not Found", &StoredObject::default()),
            }
        }
        "PUT" => {
            let key = path.trim_start_matches('/').to_owned();
            let object = if let Some(source) = headers.get("x-amz-copy-source") {
                let source = decode_url_component(source)
                    .trim_start_matches('/')
                    .to_owned();
                objects
                    .lock()
                    .expect("lock mock s3 objects")
                    .get(&source)
                    .cloned()
                    .unwrap_or_default()
            } else {
                StoredObject {
                    body,
                    metadata: headers
                        .iter()
                        .filter_map(|(name, value)| {
                            name.strip_prefix("x-amz-meta-")
                                .map(|name| (name.to_string(), value.clone()))
                        })
                        .collect(),
                }
            };
            objects
                .lock()
                .expect("lock mock s3 objects")
                .insert(key, object);
            send_response(&mut stream, 200, "OK", "application/xml", b"");
        }
        "DELETE" => {
            objects
                .lock()
                .expect("lock mock s3 objects")
                .remove(path.trim_start_matches('/'));
            send_response(&mut stream, 204, "No Content", "application/xml", b"");
        }
        "POST" if raw_query.starts_with("delete") => {
            let keys = parse_delete_objects_keys(&body);
            let bucket = path.trim_matches('/');
            let mut objects = objects.lock().expect("lock mock s3 objects");
            if keys.is_empty() {
                let bucket_prefix = format!("{bucket}/");
                objects.retain(|key, _| !key.starts_with(&bucket_prefix));
            } else {
                for key in keys {
                    objects.remove(&format!("{bucket}/{key}"));
                }
            }
            send_response(
                &mut stream,
                200,
                "OK",
                "application/xml",
                br#"<?xml version="1.0" encoding="UTF-8"?><DeleteResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"/>"#,
            );
        }
        _ => send_response(
            &mut stream,
            405,
            "Method Not Allowed",
            "text/plain",
            b"unsupported",
        ),
    }
}

fn list_objects_xml(
    path: &str,
    prefix: &str,
    delimiter: Option<&str>,
    objects: &Arc<Mutex<BTreeMap<String, StoredObject>>>,
) -> String {
    let bucket = path
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or_default();
    let full_prefix = format!("{bucket}/{prefix}");
    let delimiter = delimiter.unwrap_or_default();
    let mut contents = Vec::new();
    let mut common_prefixes = BTreeSet::new();

    for (key, object) in objects.lock().expect("lock mock s3 objects").iter() {
        let Some(relative) = key.strip_prefix(&full_prefix) else {
            continue;
        };
        if !delimiter.is_empty() {
            if let Some((first, _)) = relative.split_once(delimiter) {
                common_prefixes.insert(format!("{prefix}{first}{delimiter}"));
                continue;
            }
        }
        contents.push((
            key.strip_prefix(&format!("{bucket}/"))
                .unwrap_or(key)
                .to_owned(),
            object.body.len(),
        ));
    }

    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">"#,
    );
    xml.push_str("<IsTruncated>false</IsTruncated>");
    for (key, len) in contents {
        xml.push_str("<Contents><Key>");
        xml.push_str(&escape_xml(&key));
        xml.push_str("</Key><Size>");
        xml.push_str(&len.to_string());
        xml.push_str("</Size></Contents>");
    }
    for prefix in common_prefixes {
        xml.push_str("<CommonPrefixes><Prefix>");
        xml.push_str(&escape_xml(&prefix));
        xml.push_str("</Prefix></CommonPrefixes>");
    }
    xml.push_str("</ListBucketResult>");
    xml
}

fn apply_range(bytes: &[u8], range: Option<&str>) -> Vec<u8> {
    let Some(range) = range.and_then(|range| range.strip_prefix("bytes=")) else {
        return bytes.to_vec();
    };
    let Some((start, end)) = range.split_once('-') else {
        return bytes.to_vec();
    };
    let start = start.parse::<usize>().unwrap_or(0);
    let end = end
        .parse::<usize>()
        .unwrap_or(bytes.len().saturating_sub(1))
        .min(bytes.len().saturating_sub(1));
    if start >= bytes.len() || start > end {
        return Vec::new();
    }
    bytes[start..=end].to_vec()
}

fn send_not_found(stream: &mut TcpStream) {
    send_response(
        stream,
        404,
        "Not Found",
        "application/xml",
        br#"<Error><Code>NoSuchKey</Code><Message>missing</Message></Error>"#,
    );
}

fn send_head_response(stream: &mut TcpStream, status: u16, reason: &str, object: &StoredObject) {
    let mut response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\nx-amz-request-id: test\r\n",
        object.body.len()
    );
    for (name, value) in &object.metadata {
        response.push_str(&format!("x-amz-meta-{name}: {value}\r\n"));
    }
    response.push_str("\r\n");
    stream
        .write_all(response.as_bytes())
        .expect("write mock s3 head response");
    stream.flush().expect("flush mock s3 response");
}

fn send_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) {
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nConnection: close\r\nx-amz-request-id: test\r\n\r\n",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .expect("write mock s3 response headers");
    stream.write_all(body).expect("write mock s3 response body");
    stream.flush().expect("flush mock s3 response");
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_query(raw: &str) -> BTreeMap<String, String> {
    raw.split('&')
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode_url_component(name), decode_url_component(value))
        })
        .collect()
}

fn parse_delete_objects_keys(body: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(body);
    let mut keys = Vec::new();
    let mut rest = text.as_ref();
    while let Some((_, after_start)) = rest.split_once("<Key>") {
        let Some((key, after_end)) = after_start.split_once("</Key>") else {
            break;
        };
        keys.push(decode_url_component(key));
        rest = after_end;
    }
    keys
}

fn decode_url_component(raw: &str) -> String {
    let mut decoded = String::new();
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let code = std::str::from_utf8(&bytes[index + 1..index + 3])
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok());
            if let Some(code) = code {
                decoded.push(code as char);
                index += 3;
                continue;
            }
        }
        if bytes[index] == b'+' {
            decoded.push(' ');
        } else {
            decoded.push(bytes[index] as char);
        }
        index += 1;
    }
    decoded
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
