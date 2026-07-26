use super::*;
use crate::config::S3Config;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

fn find_http_headers_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn read_http_put(stream: &mut std::net::TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("read timeout");
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1024];
    let headers_end = loop {
        let read = stream.read(&mut chunk).expect("request bytes");
        if read == 0 {
            break request.len();
        }
        request.extend_from_slice(&chunk[..read]);
        let Some(headers_end) = find_http_headers_end(&request) else {
            continue;
        };
        let headers = String::from_utf8_lossy(&request[..headers_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        if request.len() >= headers_end + 4 + content_length {
            break headers_end;
        }
    };
    String::from_utf8_lossy(&request[..headers_end]).into_owned()
}

fn spawn_s3_put_server(statuses: Vec<&'static str>) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = listener.local_addr().expect("address");
    let handle = thread::spawn(move || {
        for status in statuses {
            let (mut stream, _) = listener.accept().expect("upload connection");
            let _ = read_http_put(&mut stream);
            let response =
                format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            stream.write_all(response.as_bytes()).expect("response");
            stream.flush().expect("flush");
        }
    });
    (format!("http://{address}"), handle)
}

fn spawn_blocking_s3_put_server(
    expected: usize,
) -> (String, Arc<AtomicUsize>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let address = listener.local_addr().expect("address");
    let accepted = Arc::new(AtomicUsize::new(0));
    let accepted_for_server = Arc::clone(&accepted);
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let release_for_server = Arc::clone(&release);
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut workers = Vec::new();
        while workers.len() < expected && Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    // On Windows an accepted socket can still behave as
                    // nonblocking when the listener is nonblocking. The test
                    // reader expects blocking semantics while request bytes
                    // arrive in multiple chunks.
                    stream
                        .set_nonblocking(false)
                        .expect("blocking upload connection");
                    accepted_for_server.fetch_add(1, Ordering::SeqCst);
                    let release = Arc::clone(&release_for_server);
                    workers.push(thread::spawn(move || {
                        let _ = read_http_put(&mut stream);
                        let (released, wake) = &*release;
                        let mut released = released.lock().expect("release lock");
                        while !*released {
                            released = wake.wait(released).expect("release wait");
                        }
                        stream
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .expect("response");
                    }));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept upload connection: {error}"),
            }
        }
        let (released, wake) = &*release_for_server;
        *released.lock().expect("release lock") = true;
        wake.notify_all();
        for worker in workers {
            worker.join().expect("upload worker");
        }
    });
    (format!("http://{address}"), accepted, handle)
}

fn spawn_s3_recording_server(
    status: &'static str,
) -> (
    String,
    std::sync::mpsc::Receiver<String>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = listener.local_addr().expect("address");
    let (request_tx, request_rx) = std::sync::mpsc::channel();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("S3 connection");
        request_tx
            .send(read_http_put(&mut stream))
            .expect("request should be recorded");
        let response =
            format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        stream.write_all(response.as_bytes()).expect("response");
        stream.flush().expect("flush");
    });
    (format!("http://{address}"), request_rx, handle)
}

fn tool_image(name: &str, data: Vec<u8>) -> ToolImageOutput {
    ToolImageOutput {
        data,
        mime_type: "image/png".into(),
        name: name.into(),
    }
}

fn test_s3(endpoint: String) -> S3Config {
    S3Config {
        endpoint,
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        access_key: "access-key".into(),
        secret_key: "secret-key".into(),
        prefix: "tool-images/".into(),
        url_expiry_secs: 3600,
        lifecycle_days: 14,
    }
}

fn minimal_png() -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    data.extend_from_slice(&[
        0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D', b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ]);
    data.extend_from_slice(&[
        0x00, 0x00, 0x00, 0x01, b'I', b'D', b'A', b'T', 0x00, 0x00, 0x00, 0x00, 0x00,
    ]);
    data.extend_from_slice(&[
        0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0x00, 0x00, 0x00, 0x00,
    ]);
    data
}

fn png_with_empty_idat_before_data() -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    data.extend_from_slice(&[
        0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D', b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ]);
    data.extend_from_slice(&[
        0x00, 0x00, 0x00, 0x00, b'I', b'D', b'A', b'T', 0x00, 0x00, 0x00, 0x00,
    ]);
    data.extend_from_slice(&[
        0x00, 0x00, 0x00, 0x01, b'I', b'D', b'A', b'T', 0x00, 0x00, 0x00, 0x00, 0x00,
    ]);
    data.extend_from_slice(&[
        0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0x00, 0x00, 0x00, 0x00,
    ]);
    data
}

fn minimal_gif() -> Vec<u8> {
    vec![
        b'G', b'I', b'F', b'8', b'9', b'a', 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00,
        0x00, 0xFF, 0xFF, 0xFF, 0x2C, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02,
        0x02, 0x4C, 0x01, 0x00, 0x3B,
    ]
}

fn minimal_webp() -> Vec<u8> {
    vec![
        b'R', b'I', b'F', b'F', 0x12, 0x00, 0x00, 0x00, b'W', b'E', b'B', b'P', b'V', b'P', b'8',
        b'L', 0x05, 0x00, 0x00, 0x00, 0x2F, 0x00, 0x00, 0x00, 0x00, 0x00,
    ]
}

fn minimal_jpeg() -> Vec<u8> {
    vec![
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x02, 0xFF, 0xC2, 0x00, 0x0B, 0x08, 0x00, 0x01, 0x00, 0x01,
        0x01, 0x01, 0x11, 0x00, 0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00, 0x11,
        0x22, 0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00, 0x33, 0x44, 0xFF, 0xD9,
    ]
}

#[tokio::test]
async fn upload_tool_images_preserves_order_and_keeps_base64_out_of_serialization() {
    let (endpoint, server) = spawn_s3_put_server(vec!["200 OK", "200 OK"]);
    let cfg = test_s3(endpoint);

    let result = upload_tool_images(
        &reqwest::Client::new(),
        &cfg,
        vec![
            tool_image("first.png", minimal_png()),
            tool_image("second.png", png_with_empty_idat_before_data()),
        ],
    )
    .await;

    server.join().expect("server");
    assert!(result.warnings.is_empty());
    assert_eq!(result.attachments.len(), 2);
    assert_eq!(result.attachments[0].name.as_deref(), Some("first.png"));
    assert_eq!(result.attachments[1].name.as_deref(), Some("second.png"));
    assert!(result.attachments.iter().all(|image| image.data.is_none()));
    let persisted = serde_json::to_string(&result.attachments).expect("serialize attachments");
    assert!(!persisted.contains("iVBOR"));
    assert!(!persisted.contains("\"data\""));
}

#[tokio::test]
async fn delete_object_uses_signed_s3_delete_request() {
    let (endpoint, request_rx, server) = spawn_s3_recording_server("204 No Content");
    let cfg = test_s3(endpoint);

    s3_delete_object(
        &reqwest::Client::new(),
        &cfg,
        "tool-images/2026-07-25/cleanup.png",
    )
    .await
    .expect("cleanup request should succeed");

    server.join().expect("server");
    let request = request_rx.recv().expect("request");
    assert!(request.starts_with("DELETE /bucket/tool-images/2026-07-25/cleanup.png HTTP/1.1"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: aws4-hmac-sha256")
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("x-amz-content-sha256:")
    );
}

#[tokio::test]
async fn upload_tool_image_groups_preserves_tool_association_and_global_limit() {
    let (endpoint, server) = spawn_s3_put_server(vec!["200 OK", "200 OK"]);
    let cfg = test_s3(endpoint);

    let result = upload_tool_image_groups(
        &reqwest::Client::new(),
        &cfg,
        vec![
            vec![tool_image("first.png", minimal_png())],
            vec![
                tool_image("second.png", png_with_empty_idat_before_data()),
                tool_image("skipped.png", minimal_png()),
            ],
        ],
        2,
    )
    .await;

    server.join().expect("server");
    assert_eq!(result.attachments.len(), 2);
    assert_eq!(result.attachments[0][0].name.as_deref(), Some("first.png"));
    assert_eq!(result.attachments[1][0].name.as_deref(), Some("second.png"));
    assert!(result.warnings[0].is_empty());
    assert_eq!(result.warnings[1].len(), 1);
    assert!(result.warnings[1][0].contains("at most 2"));
}

#[tokio::test]
async fn upload_tool_image_groups_shares_concurrency_across_tools() {
    let (endpoint, accepted, server) = spawn_blocking_s3_put_server(3);
    let cfg = test_s3(endpoint);

    let result = upload_tool_image_groups(
        &reqwest::Client::new(),
        &cfg,
        vec![
            vec![tool_image("first.png", minimal_png())],
            vec![tool_image("second.png", minimal_png())],
            vec![tool_image("third.png", minimal_png())],
        ],
        3,
    )
    .await;

    server.join().expect("server");
    assert_eq!(accepted.load(Ordering::SeqCst), 3);
    assert!(result.warnings.iter().all(Vec::is_empty));
    assert_eq!(result.attachments.iter().map(Vec::len).sum::<usize>(), 3);
}

#[tokio::test]
async fn upload_tool_images_keeps_successes_when_one_upload_fails() {
    let (endpoint, server) = spawn_s3_put_server(vec!["500 Internal Server Error", "200 OK"]);
    let cfg = test_s3(endpoint);

    let result = upload_tool_images(
        &reqwest::Client::new(),
        &cfg,
        vec![
            tool_image("first.png", minimal_png()),
            tool_image("second.png", png_with_empty_idat_before_data()),
        ],
    )
    .await;

    server.join().expect("server");
    assert_eq!(result.attachments.len(), 1);
    assert_eq!(result.warnings.len(), 1);
    assert!(result.warnings[0].contains("not attached"));
}

#[tokio::test]
async fn upload_tool_images_rejects_invalid_content_without_contacting_s3() {
    let cfg = test_s3("http://127.0.0.1:1".into());
    let result = upload_tool_images(
        &reqwest::Client::new(),
        &cfg,
        vec![tool_image("fake.png", b"not an image".to_vec())],
    )
    .await;

    assert!(result.attachments.is_empty());
    assert_eq!(result.warnings.len(), 1);
    assert!(result.warnings[0].contains("valid PNG or JPEG"));
}

#[test]
fn s3_presigned_get_url_clamps_expiry_for_aws_endpoints() {
    let cfg = S3Config {
        endpoint: "https://s3.amazonaws.com".into(),
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        access_key: "access-key".into(),
        secret_key: "secret-key".into(),
        prefix: "images/".into(),
        url_expiry_secs: 1_209_600,
        lifecycle_days: 14,
    };

    let url = s3_presigned_get_url(&cfg, "sample.png").expect("presigned url should be built");
    let parsed = reqwest::Url::parse(&url).expect("presigned url should parse");
    let expires = parsed
        .query_pairs()
        .find(|(key, _)| key == "X-Amz-Expires")
        .map(|(_, value)| value.into_owned())
        .expect("X-Amz-Expires should exist");

    assert_eq!(expires, "604800");
}

#[test]
fn s3_presigned_get_url_preserves_expiry_for_compatible_gateways() {
    let cfg = S3Config {
        endpoint: "https://minio.example.test".into(),
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        access_key: "access-key".into(),
        secret_key: "secret-key".into(),
        prefix: "images/".into(),
        url_expiry_secs: 1_209_600,
        lifecycle_days: 14,
    };

    let url = s3_presigned_get_url(&cfg, "sample.png").expect("presigned url should be built");
    let parsed = reqwest::Url::parse(&url).expect("presigned url should parse");
    let expires = parsed
        .query_pairs()
        .find(|(key, _)| key == "X-Amz-Expires")
        .map(|(_, value)| value.into_owned())
        .expect("X-Amz-Expires should exist");

    assert_eq!(expires, "1209600");
}

#[test]
fn s3_presigned_get_url_clamps_expiry_for_aws_china_endpoints() {
    let cfg = S3Config {
        endpoint: "https://s3.cn-north-1.amazonaws.com.cn".into(),
        region: "cn-north-1".into(),
        bucket: "bucket".into(),
        access_key: "access-key".into(),
        secret_key: "secret-key".into(),
        prefix: "images/".into(),
        url_expiry_secs: 1_209_600,
        lifecycle_days: 14,
    };

    let url = s3_presigned_get_url(&cfg, "sample.png").expect("presigned url should be built");
    let parsed = reqwest::Url::parse(&url).expect("presigned url should parse");
    let expires = parsed
        .query_pairs()
        .find(|(key, _)| key == "X-Amz-Expires")
        .map(|(_, value)| value.into_owned())
        .expect("X-Amz-Expires should exist");

    assert_eq!(expires, "604800");
}

#[test]
fn attachment_object_key_tokens_round_trip() {
    let cfg = S3Config {
        endpoint: "https://minio.example.test/storage".into(),
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        access_key: "access-key".into(),
        secret_key: "secret-key".into(),
        prefix: "images/".into(),
        url_expiry_secs: 3600,
        lifecycle_days: 14,
    };
    let object_key = "images/2026/demo.png";
    let token = sign_attachment_object_key(&cfg, object_key);

    assert!(verify_attachment_object_key(&cfg, object_key, &token));
    assert!(!verify_attachment_object_key(
        &cfg,
        "images/2026/other.png",
        &token,
    ));
}

#[test]
fn s3_config_identity_is_stable_and_changes_with_storage_settings() {
    let cfg = S3Config {
        endpoint: "https://minio.example.test/storage".into(),
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        access_key: "access-key".into(),
        secret_key: "secret-key".into(),
        prefix: "images/".into(),
        url_expiry_secs: 3600,
        lifecycle_days: 14,
    };

    assert_eq!(s3_config_id(&cfg), s3_config_id(&cfg.clone()));

    let mut changed_endpoint = cfg.clone();
    changed_endpoint.endpoint = "https://replacement.example.test/storage".into();
    assert_ne!(s3_config_id(&cfg), s3_config_id(&changed_endpoint));

    let mut changed_secret = cfg.clone();
    changed_secret.secret_key = "replacement-secret-key".into();
    assert_ne!(s3_config_id(&cfg), s3_config_id(&changed_secret));
}

#[test]
fn attachment_tokens_are_bound_to_the_full_s3_configuration() {
    let cfg = S3Config {
        endpoint: "https://minio.example.test/storage".into(),
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        access_key: "access-key".into(),
        secret_key: "secret-key".into(),
        prefix: "images/".into(),
        url_expiry_secs: 3600,
        lifecycle_days: 14,
    };
    let object_key = "images/2026/demo.png";
    let token = sign_attachment_object_key(&cfg, object_key);
    let mut changed_cfg = cfg.clone();
    changed_cfg.endpoint = "https://replacement.example.test/storage".into();

    assert!(!verify_attachment_object_key(
        &changed_cfg,
        object_key,
        &token
    ));
}

#[test]
fn resolve_image_url_presigns_uploaded_s3_objects() {
    let cfg = S3Config {
        endpoint: "https://minio.example.test/storage".into(),
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        access_key: "access-key".into(),
        secret_key: "secret-key".into(),
        prefix: "images/".into(),
        url_expiry_secs: 3600,
        lifecycle_days: 14,
    };

    let url = resolve_image_url(
        "https://expired.example.test/old.png",
        Some("images/2026/demo.png"),
        Some(&cfg),
    )
    .expect("s3 object key should resolve to fresh presigned url");

    assert!(url.starts_with("https://minio.example.test/storage/bucket/images/2026/demo.png?"));
    assert!(url.contains("X-Amz-Signature="));
}

#[test]
fn canonical_uri_from_url_preserves_endpoint_path_prefix() {
    let parsed =
        reqwest::Url::parse("https://minio.example.test/storage/v1/bucket/images/demo.png")
            .expect("url should parse");

    assert_eq!(
        canonical_uri_from_url(&parsed),
        "/storage/v1/bucket/images/demo.png"
    );
}

#[test]
fn merge_s3_lifecycle_configuration_creates_rule_document() {
    let cfg = S3Config {
        endpoint: "https://s3.amazonaws.com".into(),
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        access_key: "access-key".into(),
        secret_key: "secret-key".into(),
        prefix: "lingclaw/images/".into(),
        url_expiry_secs: 604_800,
        lifecycle_days: 14,
    };

    let xml = merge_s3_lifecycle_configuration(None, &cfg)
        .expect("lifecycle configuration should be generated");

    assert!(xml.contains("<LifecycleConfiguration"));
    assert!(xml.contains("<Prefix>lingclaw/images/</Prefix>"));
    assert!(xml.contains("<Days>14</Days>"));
}

#[test]
fn merge_s3_lifecycle_configuration_replaces_existing_lingclaw_rule() {
    let cfg = S3Config {
        endpoint: "https://s3.amazonaws.com".into(),
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        access_key: "access-key".into(),
        secret_key: "secret-key".into(),
        prefix: "lingclaw/images/".into(),
        url_expiry_secs: 604_800,
        lifecycle_days: 14,
    };
    let rule_id = s3_lifecycle_rule_id(&cfg);
    let existing = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><LifecycleConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Rule><ID>{rule_id}</ID><Status>Enabled</Status><Filter><Prefix>lingclaw/images/</Prefix></Filter><Expiration><Days>7</Days></Expiration></Rule></LifecycleConfiguration>"
    );

    let xml = merge_s3_lifecycle_configuration(Some(&existing), &cfg)
        .expect("lifecycle configuration should be updated");

    assert!(xml.contains(&format!("<ID>{rule_id}</ID>")));
    assert!(xml.contains("<Days>14</Days>"));
    assert!(!xml.contains("<Days>7</Days>"));
}

#[test]
fn merge_s3_lifecycle_configuration_preserves_unrelated_rules() {
    let cfg = S3Config {
        endpoint: "https://s3.amazonaws.com".into(),
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        access_key: "access-key".into(),
        secret_key: "secret-key".into(),
        prefix: "lingclaw/images/".into(),
        url_expiry_secs: 604_800,
        lifecycle_days: 14,
    };
    let rule_id = s3_lifecycle_rule_id(&cfg);
    let existing = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><LifecycleConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Rule><ID>UnrelatedRule</ID><Status>Enabled</Status><Filter><Prefix>archive/</Prefix></Filter><Expiration><Days>30</Days></Expiration></Rule><Rule><ID>{rule_id}</ID><Status>Enabled</Status><Filter><Prefix>lingclaw/images/</Prefix></Filter><Expiration><Days>7</Days></Expiration></Rule></LifecycleConfiguration>"
    );

    let xml = merge_s3_lifecycle_configuration(Some(&existing), &cfg)
        .expect("lifecycle configuration should preserve unrelated rules");

    assert!(xml.contains("<ID>UnrelatedRule</ID>"));
    assert!(xml.contains("<Prefix>archive/</Prefix>"));
    assert!(xml.contains(&format!("<ID>{rule_id}</ID>")));
    assert!(xml.contains("<Days>14</Days>"));
    assert_eq!(xml.matches("<Rule>").count(), 2);
}

#[test]
fn s3_rule_matches_cfg_accepts_equivalent_rule_with_whitespace() {
    let cfg = S3Config {
        endpoint: "https://s3.amazonaws.com".into(),
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        access_key: "access-key".into(),
        secret_key: "secret-key".into(),
        prefix: "lingclaw/images/".into(),
        url_expiry_secs: 604_800,
        lifecycle_days: 14,
    };
    let rule_id = s3_lifecycle_rule_id(&cfg);
    let rule = format!(
        "<Rule>\n  <ID>{rule_id}</ID>\n  <Status> Enabled </Status>\n  <Filter>\n    <Prefix>lingclaw/images/</Prefix>\n  </Filter>\n  <Expiration>\n    <Days>14</Days>\n  </Expiration>\n</Rule>"
    );

    assert!(s3_rule_matches_cfg(&rule, &cfg));
}

#[test]
fn s3_rule_matches_cfg_rejects_complex_filter_rules() {
    let cfg = S3Config {
        endpoint: "https://s3.amazonaws.com".into(),
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        access_key: "access-key".into(),
        secret_key: "secret-key".into(),
        prefix: "lingclaw/images/".into(),
        url_expiry_secs: 604_800,
        lifecycle_days: 14,
    };
    let rule_id = s3_lifecycle_rule_id(&cfg);
    let rule = format!(
        "<Rule><ID>{rule_id}</ID><Status>Enabled</Status><Filter><And><Prefix>lingclaw/images/</Prefix><Tag><Key>kind</Key><Value>temp</Value></Tag></And></Filter><Expiration><Days>14</Days></Expiration></Rule>"
    );

    assert!(!s3_rule_matches_cfg(&rule, &cfg));
}

#[test]
fn s3_rule_matches_cfg_preserves_spaces_in_prefix_content() {
    let cfg = S3Config {
        endpoint: "https://s3.amazonaws.com".into(),
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        access_key: "access-key".into(),
        secret_key: "secret-key".into(),
        prefix: "lingclaw/images with spaces/".into(),
        url_expiry_secs: 604_800,
        lifecycle_days: 14,
    };
    let rule_id = s3_lifecycle_rule_id(&cfg);
    let rule = format!(
        "<Rule><ID>{rule_id}</ID><Status>Enabled</Status><Filter><Prefix>lingclaw/images with spaces/</Prefix></Filter><Expiration><Days>14</Days></Expiration></Rule>"
    );

    assert!(s3_rule_matches_cfg(&rule, &cfg));
}

#[test]
fn s3_rule_matches_cfg_handles_xml_entity_escaped_prefix() {
    let cfg = S3Config {
        endpoint: "https://s3.amazonaws.com".into(),
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        access_key: "access-key".into(),
        secret_key: "secret-key".into(),
        prefix: "lingclaw/A&B<C>/".into(),
        url_expiry_secs: 604_800,
        lifecycle_days: 14,
    };
    let rule_id = s3_lifecycle_rule_id(&cfg);
    let rule = format!(
        "<Rule><ID>{rule_id}</ID><Status>Enabled</Status><Filter><Prefix>lingclaw/A&amp;B&lt;C&gt;/</Prefix></Filter><Expiration><Days>14</Days></Expiration></Rule>"
    );

    assert!(s3_rule_matches_cfg(&rule, &cfg));
}

#[test]
fn s3_lifecycle_rule_id_is_stable_for_prefix() {
    let cfg = S3Config {
        endpoint: "https://s3.amazonaws.com".into(),
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        access_key: "access-key".into(),
        secret_key: "secret-key".into(),
        prefix: "lingclaw/images/".into(),
        url_expiry_secs: 604_800,
        lifecycle_days: 14,
    };

    assert_eq!(
        s3_lifecycle_rule_id(&cfg),
        "LingClawTempImages-c023dbf3865822ad"
    );
}

#[test]
fn detect_image_upload_content_type_recognizes_common_formats() {
    assert_eq!(
        detect_image_upload_content_type(&minimal_png()),
        Some("image/png")
    );
    assert_eq!(
        detect_image_upload_content_type(&png_with_empty_idat_before_data()),
        Some("image/png")
    );
    assert_eq!(
        detect_image_upload_content_type(&minimal_jpeg()),
        Some("image/jpeg")
    );
}

#[test]
fn detect_image_upload_content_type_rejects_non_images() {
    assert_eq!(detect_image_upload_content_type(b"not an image"), None);
}

#[test]
fn detect_image_upload_content_type_rejects_truncated_images() {
    assert_eq!(detect_image_upload_content_type(&minimal_png()[..8]), None);
    assert_eq!(detect_image_upload_content_type(&minimal_jpeg()[..4]), None);
}

#[test]
fn detect_image_upload_content_type_rejects_unsupported_formats() {
    assert_eq!(detect_image_upload_content_type(&minimal_gif()), None);
    assert_eq!(detect_image_upload_content_type(&minimal_webp()), None);
}

#[test]
fn supported_image_content_type_allows_only_png_and_jpeg() {
    assert!(is_supported_image_content_type("image/jpeg"));
    assert!(is_supported_image_content_type("image/jpg"));
    assert!(is_supported_image_content_type("image/png; charset=binary"));
    assert!(!is_supported_image_content_type("image/gif"));
    assert!(!is_supported_image_content_type("image/webp"));
    assert!(!is_supported_image_content_type("text/html"));
    assert!(!is_supported_image_content_type("image/svg+xml"));
    assert!(!is_supported_image_content_type(""));
}
