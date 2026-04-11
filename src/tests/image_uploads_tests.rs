use super::*;
use crate::config::S3Config;

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
