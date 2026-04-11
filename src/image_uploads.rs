use base64::Engine;
use reqwest::Client;

use crate::config::S3Config;
use hmac::{Hmac, Mac};
use md5::Md5;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub(crate) fn is_supported_image_content_type(content_type: &str) -> bool {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    matches!(mime.as_str(), "image/jpeg" | "image/jpg" | "image/png")
}

fn sha256_hash(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex_encode(&hasher.finalize())
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC-SHA256 accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(input: &str) -> Option<Vec<u8>> {
    if !input.len().is_multiple_of(2) {
        return None;
    }

    let mut bytes = Vec::with_capacity(input.len() / 2);
    let mut chars = input.as_bytes().chunks_exact(2);
    for pair in &mut chars {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        bytes.push(((hi << 4) | lo) as u8);
    }
    Some(bytes)
}

fn attachment_signing_payload(cfg: &S3Config, object_key: &str) -> String {
    format!("lingclaw-upload:{}:{}", cfg.bucket, object_key)
}

pub(crate) fn sign_attachment_object_key(cfg: &S3Config, object_key: &str) -> String {
    let payload = attachment_signing_payload(cfg, object_key);
    hex_encode(&hmac_sha256(cfg.secret_key.as_bytes(), payload.as_bytes()))
}

pub(crate) fn verify_attachment_object_key(cfg: &S3Config, object_key: &str, token: &str) -> bool {
    let Some(signature) = hex_decode(token) else {
        return false;
    };
    let payload = attachment_signing_payload(cfg, object_key);
    let mut mac = HmacSha256::new_from_slice(cfg.secret_key.as_bytes())
        .expect("HMAC-SHA256 accepts any key length");
    mac.update(payload.as_bytes());
    mac.verify_slice(&signature).is_ok()
}

pub(crate) fn resolve_image_url(
    fallback_url: &str,
    s3_object_key: Option<&str>,
    s3_cfg: Option<&S3Config>,
) -> Result<String, String> {
    if let Some(object_key) = s3_object_key.filter(|key| !key.trim().is_empty())
        && let Some(cfg) = s3_cfg
    {
        return s3_presigned_get_url(cfg, object_key);
    }

    if fallback_url.is_empty() {
        Err("Image attachment is missing a usable URL".to_string())
    } else {
        Ok(fallback_url.to_string())
    }
}

fn derive_signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let date_key = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let region_key = hmac_sha256(&date_key, region.as_bytes());
    let service_key = hmac_sha256(&region_key, service.as_bytes());
    hmac_sha256(&service_key, b"aws4_request")
}

fn uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b'/' if !encode_slash => out.push('/'),
            _ => {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

fn s3_host_with_port(parsed: &reqwest::Url) -> String {
    match parsed.port() {
        Some(port) => format!("{}:{}", parsed.host_str().unwrap_or(""), port),
        None => parsed.host_str().unwrap_or("").to_string(),
    }
}

fn canonical_uri_from_url(parsed: &reqwest::Url) -> String {
    let path = parsed.path();
    if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    }
}

fn s3_authorization_header(
    cfg: &S3Config,
    date_stamp: &str,
    amz_date: &str,
    canonical_request: &str,
    signed_headers: &str,
) -> String {
    let credential_scope = format!("{date_stamp}/{}/s3/aws4_request", cfg.region);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        sha256_hash(canonical_request.as_bytes())
    );

    let signing_key = derive_signing_key(&cfg.secret_key, date_stamp, &cfg.region, "s3");
    let signature = hex_encode(&hmac_sha256(&signing_key, string_to_sign.as_bytes()));

    format!(
        "AWS4-HMAC-SHA256 Credential={}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}",
        cfg.access_key
    )
}

fn is_aws_s3_endpoint(endpoint: &str) -> bool {
    reqwest::Url::parse(endpoint)
        .ok()
        .and_then(|url| {
            url.host_str().map(|host| {
                let host = host.to_ascii_lowercase();
                host == "s3.amazonaws.com"
                    || (host.starts_with("s3.")
                        && (host.ends_with(".amazonaws.com")
                            || host.ends_with(".amazonaws.com.cn")))
            })
        })
        .unwrap_or(false)
}

fn effective_s3_url_expiry_secs(cfg: &S3Config) -> u64 {
    let requested = cfg.url_expiry_secs.max(1);
    if is_aws_s3_endpoint(&cfg.endpoint) {
        requested.min(604_800)
    } else {
        requested
    }
}

fn s3_lifecycle_rule_id(cfg: &S3Config) -> String {
    let prefix_hash = sha256_hash(cfg.prefix.trim_start_matches('/').as_bytes());
    format!("LingClawTempImages-{}", &prefix_hash[..16])
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn s3_lifecycle_rule_xml(cfg: &S3Config) -> String {
    let rule_id = xml_escape(&s3_lifecycle_rule_id(cfg));
    let prefix = xml_escape(cfg.prefix.trim_start_matches('/'));
    format!(
        "<Rule><ID>{rule_id}</ID><Status>Enabled</Status><Filter><Prefix>{prefix}</Prefix></Filter><Expiration><Days>{}</Days></Expiration></Rule>",
        cfg.lifecycle_days
    )
}

fn s3_extract_tag_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim().to_string())
}

fn s3_extract_section<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(&xml[start..end])
}

fn s3_matches_simple_text_section(xml: &str, tag: &str, expected: &str) -> bool {
    let trimmed = xml.trim();
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    trimmed.starts_with(&open)
        && trimmed.ends_with(&close)
        && s3_extract_tag_text(trimmed, tag).as_deref() == Some(expected)
}

fn s3_extract_rule_id(rule_xml: &str) -> Option<String> {
    s3_extract_tag_text(rule_xml, "ID")
}

fn s3_rule_matches_cfg(rule_xml: &str, cfg: &S3Config) -> bool {
    let Some(filter) = s3_extract_section(rule_xml, "Filter") else {
        return false;
    };
    let Some(expiration) = s3_extract_section(rule_xml, "Expiration") else {
        return false;
    };
    let rule_id = s3_lifecycle_rule_id(cfg);
    let expected_filter = xml_escape(cfg.prefix.trim_start_matches('/'));
    let expected_expiration = cfg.lifecycle_days.to_string();

    s3_extract_rule_id(rule_xml).as_deref() == Some(rule_id.as_str())
        && s3_extract_tag_text(rule_xml, "Status")
            .map(|status| status.eq_ignore_ascii_case("Enabled"))
            .unwrap_or(false)
        && s3_matches_simple_text_section(filter, "Prefix", &expected_filter)
        && s3_matches_simple_text_section(expiration, "Days", &expected_expiration)
}

fn s3_find_rule_ranges(xml: &str) -> Result<Vec<(usize, usize)>, String> {
    let mut ranges = Vec::new();
    let mut search_from = 0;

    while let Some(rel_start) = xml[search_from..].find("<Rule>") {
        let start = search_from + rel_start;
        let body_start = start + "<Rule>".len();
        let rel_end = xml[body_start..].find("</Rule>").ok_or_else(|| {
            "Invalid S3 lifecycle configuration XML: unterminated Rule element".to_string()
        })?;
        let end = body_start + rel_end + "</Rule>".len();
        ranges.push((start, end));
        search_from = end;
    }

    Ok(ranges)
}

fn merge_s3_lifecycle_configuration(
    existing: Option<&str>,
    cfg: &S3Config,
) -> Result<String, String> {
    let desired_rule = s3_lifecycle_rule_xml(cfg);
    let rule_id = s3_lifecycle_rule_id(cfg);

    match existing {
        Some(xml) => {
            for (start, end) in s3_find_rule_ranges(xml)? {
                if s3_extract_rule_id(&xml[start..end]).as_deref() == Some(rule_id.as_str()) {
                    let mut merged =
                        String::with_capacity(xml.len() - (end - start) + desired_rule.len());
                    merged.push_str(&xml[..start]);
                    merged.push_str(&desired_rule);
                    merged.push_str(&xml[end..]);
                    return Ok(merged);
                }
            }

            if let Some(idx) = xml.rfind("</LifecycleConfiguration>") {
                let mut merged = String::with_capacity(xml.len() + desired_rule.len());
                merged.push_str(&xml[..idx]);
                merged.push_str(&desired_rule);
                merged.push_str(&xml[idx..]);
                return Ok(merged);
            }

            Err(
                "Invalid S3 lifecycle configuration XML: missing closing LifecycleConfiguration tag"
                    .to_string(),
            )
        }
        None => Ok(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><LifecycleConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">{desired_rule}</LifecycleConfiguration>"
        )),
    }
}

async fn s3_get_bucket_lifecycle_xml(
    http: &Client,
    cfg: &S3Config,
) -> Result<Option<String>, String> {
    let now = chrono::Utc::now();
    let date_stamp = now.format("%Y%m%d").to_string();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let empty_sha256 = sha256_hash(&[]);

    let url = format!("{}/{}?lifecycle", cfg.endpoint, cfg.bucket);
    let parsed = reqwest::Url::parse(&url).map_err(|e| format!("Invalid S3 lifecycle URL: {e}"))?;
    let host = s3_host_with_port(&parsed);
    let canonical_uri = canonical_uri_from_url(&parsed);
    let canonical_querystring = "lifecycle=";
    let canonical_headers =
        format!("host:{host}\nx-amz-content-sha256:{empty_sha256}\nx-amz-date:{amz_date}\n");
    let signed_headers = "host;x-amz-content-sha256;x-amz-date";
    let canonical_request = format!(
        "GET\n{canonical_uri}\n{canonical_querystring}\n{canonical_headers}\n{signed_headers}\n{empty_sha256}"
    );
    let authorization = s3_authorization_header(
        cfg,
        &date_stamp,
        &amz_date,
        &canonical_request,
        signed_headers,
    );

    let resp = http
        .get(&url)
        .header("x-amz-content-sha256", &empty_sha256)
        .header("x-amz-date", &amz_date)
        .header("Authorization", &authorization)
        .send()
        .await
        .map_err(|e| format!("S3 lifecycle GET failed: {e}"))?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let truncated = crate::truncate(&body, 512);
        return Err(format!(
            "S3 lifecycle GET failed (HTTP {status}): {truncated}"
        ));
    }

    Ok(Some(resp.text().await.unwrap_or_default()))
}

async fn s3_put_bucket_lifecycle_xml(
    http: &Client,
    cfg: &S3Config,
    xml: &str,
) -> Result<(), String> {
    let now = chrono::Utc::now();
    let date_stamp = now.format("%Y%m%d").to_string();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let payload_hash = sha256_hash(xml.as_bytes());
    let content_md5 = base64::engine::general_purpose::STANDARD.encode(Md5::digest(xml.as_bytes()));

    let url = format!("{}/{}?lifecycle", cfg.endpoint, cfg.bucket);
    let parsed = reqwest::Url::parse(&url).map_err(|e| format!("Invalid S3 lifecycle URL: {e}"))?;
    let host = s3_host_with_port(&parsed);
    let canonical_uri = canonical_uri_from_url(&parsed);
    let canonical_querystring = "lifecycle=";
    let canonical_headers = format!(
        "content-md5:{content_md5}\ncontent-type:application/xml\nhost:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n"
    );
    let signed_headers = "content-md5;content-type;host;x-amz-content-sha256;x-amz-date";
    let canonical_request = format!(
        "PUT\n{canonical_uri}\n{canonical_querystring}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );
    let authorization = s3_authorization_header(
        cfg,
        &date_stamp,
        &amz_date,
        &canonical_request,
        signed_headers,
    );

    let resp = http
        .put(&url)
        .header("Content-Type", "application/xml")
        .header("Content-MD5", &content_md5)
        .header("x-amz-content-sha256", &payload_hash)
        .header("x-amz-date", &amz_date)
        .header("Authorization", &authorization)
        .body(xml.to_string())
        .send()
        .await
        .map_err(|e| format!("S3 lifecycle PUT failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let truncated = crate::truncate(&body, 512);
        return Err(format!(
            "S3 lifecycle PUT failed (HTTP {status}): {truncated}"
        ));
    }

    Ok(())
}

pub(crate) async fn ensure_s3_temp_image_lifecycle(
    http: &Client,
    cfg: &S3Config,
) -> Result<bool, String> {
    if cfg.lifecycle_days == 0 {
        return Ok(false);
    }

    let existing = s3_get_bucket_lifecycle_xml(http, cfg).await?;
    if let Some(existing_xml) = existing.as_deref() {
        for (start, end) in s3_find_rule_ranges(existing_xml)? {
            if s3_rule_matches_cfg(&existing_xml[start..end], cfg) {
                return Ok(false);
            }
        }
    }

    let desired = merge_s3_lifecycle_configuration(existing.as_deref(), cfg)?;
    s3_put_bucket_lifecycle_xml(http, cfg, &desired).await?;
    Ok(true)
}

fn content_type_to_ext(ct: &str) -> &str {
    let mime = ct.split(';').next().unwrap_or("").trim();
    match mime {
        "image/png" => "png",
        _ => "jpg",
    }
}

pub(crate) async fn s3_put_object(
    http: &Client,
    cfg: &S3Config,
    object_key: &str,
    data: &[u8],
    content_type: &str,
) -> Result<(), String> {
    let now = chrono::Utc::now();
    let date_stamp = now.format("%Y%m%d").to_string();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();

    let url = format!(
        "{}/{}/{}",
        cfg.endpoint,
        cfg.bucket,
        uri_encode(object_key, false)
    );
    let parsed = reqwest::Url::parse(&url).map_err(|e| format!("Invalid S3 URL: {e}"))?;
    let host = s3_host_with_port(&parsed);
    let content_sha256 = sha256_hash(data);

    let canonical_uri = canonical_uri_from_url(&parsed);
    let canonical_headers = format!(
        "content-type:{content_type}\nhost:{host}\nx-amz-content-sha256:{content_sha256}\nx-amz-date:{amz_date}\n"
    );
    let signed_headers = "content-type;host;x-amz-content-sha256;x-amz-date";

    let canonical_request =
        format!("PUT\n{canonical_uri}\n\n{canonical_headers}\n{signed_headers}\n{content_sha256}");

    let authorization = s3_authorization_header(
        cfg,
        &date_stamp,
        &amz_date,
        &canonical_request,
        signed_headers,
    );

    let resp = http
        .put(&url)
        .header("Content-Type", content_type)
        .header("x-amz-content-sha256", &content_sha256)
        .header("x-amz-date", &amz_date)
        .header("Authorization", &authorization)
        .body(data.to_vec())
        .send()
        .await
        .map_err(|e| format!("S3 PUT failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let truncated = crate::truncate(&body, 512);
        return Err(format!("S3 PUT failed (HTTP {status}): {truncated}"));
    }
    Ok(())
}

pub(crate) fn s3_presigned_get_url(cfg: &S3Config, object_key: &str) -> Result<String, String> {
    let now = chrono::Utc::now();
    let date_stamp = now.format("%Y%m%d").to_string();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let expires = effective_s3_url_expiry_secs(cfg);

    let base_url = format!(
        "{}/{}/{}",
        cfg.endpoint,
        cfg.bucket,
        uri_encode(object_key, false)
    );
    let parsed = reqwest::Url::parse(&base_url).map_err(|e| format!("Invalid S3 URL: {e}"))?;
    let host = s3_host_with_port(&parsed);

    let credential_scope = format!("{date_stamp}/{}/s3/aws4_request", cfg.region);
    let credential = format!("{}/{credential_scope}", cfg.access_key);

    let canonical_uri = canonical_uri_from_url(&parsed);
    let canonical_querystring = format!(
        "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential={}&X-Amz-Date={}&X-Amz-Expires={}&X-Amz-SignedHeaders=host",
        uri_encode(&credential, true),
        amz_date,
        expires
    );
    let canonical_headers = format!("host:{host}\n");
    let signed_headers = "host";

    let canonical_request = format!(
        "GET\n{canonical_uri}\n{canonical_querystring}\n{canonical_headers}\n{signed_headers}\nUNSIGNED-PAYLOAD"
    );

    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        sha256_hash(canonical_request.as_bytes())
    );

    let signing_key = derive_signing_key(&cfg.secret_key, &date_stamp, &cfg.region, "s3");
    let signature = hex_encode(&hmac_sha256(&signing_key, string_to_sign.as_bytes()));

    Ok(format!(
        "{base_url}?{canonical_querystring}&X-Amz-Signature={signature}"
    ))
}

pub(crate) fn generate_s3_object_key(cfg: &S3Config, content_type: &str, data: &[u8]) -> String {
    let ext = content_type_to_ext(content_type);
    let now = chrono::Utc::now();
    let date_part = now.format("%Y-%m-%d").to_string();
    let ts = now.timestamp_millis();
    let hash = &sha256_hash(data)[..12];
    let prefix = cfg.prefix.trim_end_matches('/');
    format!("{prefix}/{date_part}/{ts}-{hash}.{ext}")
}

fn read_be_u16(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes([
        *data.get(offset)?,
        *data.get(offset + 1)?,
    ]))
}

fn read_be_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *data.get(offset)?,
        *data.get(offset + 1)?,
        *data.get(offset + 2)?,
        *data.get(offset + 3)?,
    ]))
}

fn is_valid_png(data: &[u8]) -> bool {
    const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    if data.len() < 8 + 12 + 12 || data[..8] != PNG_SIGNATURE {
        return false;
    }

    let mut offset = 8;
    let mut saw_ihdr = false;
    let mut total_idat_bytes = 0usize;

    while offset + 12 <= data.len() {
        let chunk_len = match read_be_u32(data, offset) {
            Some(length) => length as usize,
            None => return false,
        };
        let Some(chunk_type) = data.get(offset + 4..offset + 8) else {
            return false;
        };
        let chunk_data_start = offset + 8;
        let Some(chunk_data_end) = chunk_data_start.checked_add(chunk_len) else {
            return false;
        };
        let Some(chunk_end) = chunk_data_end.checked_add(4) else {
            return false;
        };
        if chunk_end > data.len() {
            return false;
        }

        match chunk_type {
            b"IHDR" => {
                if saw_ihdr || offset != 8 || chunk_len != 13 {
                    return false;
                }
                let width = read_be_u32(data, chunk_data_start).unwrap_or(0);
                let height = read_be_u32(data, chunk_data_start + 4).unwrap_or(0);
                if width == 0 || height == 0 {
                    return false;
                }
                saw_ihdr = true;
            }
            b"IDAT" => {
                if !saw_ihdr {
                    return false;
                }
                let Some(next_total) = total_idat_bytes.checked_add(chunk_len) else {
                    return false;
                };
                total_idat_bytes = next_total;
            }
            b"IEND" => {
                return saw_ihdr
                    && total_idat_bytes > 0
                    && chunk_len == 0
                    && chunk_end == data.len();
            }
            _ => {}
        }

        offset = chunk_end;
    }

    false
}

fn is_jpeg_sof_marker(marker: u8) -> bool {
    matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF)
}

fn find_next_jpeg_marker(data: &[u8], mut offset: usize) -> Option<usize> {
    while offset + 1 < data.len() {
        if data[offset] != 0xFF {
            offset += 1;
            continue;
        }

        let marker_offset = offset;
        offset += 1;
        while offset < data.len() && data[offset] == 0xFF {
            offset += 1;
        }

        let marker = *data.get(offset)?;
        match marker {
            0x00 | 0xD0..=0xD7 => offset += 1,
            _ => return Some(marker_offset),
        }
    }

    None
}

fn is_valid_jpeg(data: &[u8]) -> bool {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return false;
    }

    let mut offset = 2;
    let mut saw_frame = false;
    let mut saw_scan = false;

    while offset < data.len() {
        if data[offset] != 0xFF {
            return false;
        }
        while offset < data.len() && data[offset] == 0xFF {
            offset += 1;
        }
        let Some(&marker) = data.get(offset) else {
            return false;
        };
        offset += 1;

        match marker {
            0xD9 => return saw_frame && saw_scan && offset == data.len(),
            0x00 | 0xD0..=0xD7 => {
                if !saw_scan {
                    return false;
                }
            }
            0x01 => {}
            _ => {
                let Some(segment_len) = read_be_u16(data, offset).map(|value| value as usize)
                else {
                    return false;
                };
                if segment_len < 2 {
                    return false;
                }
                let segment_start = offset + 2;
                let Some(segment_end) = offset.checked_add(segment_len) else {
                    return false;
                };
                if segment_end > data.len() {
                    return false;
                }

                if is_jpeg_sof_marker(marker) {
                    let components = usize::from(*data.get(segment_start + 5).unwrap_or(&0));
                    let width = read_be_u16(data, segment_start + 3).unwrap_or(0);
                    let height = read_be_u16(data, segment_start + 1).unwrap_or(0);
                    if components == 0
                        || width == 0
                        || height == 0
                        || segment_end < segment_start + 6 + (components * 3)
                    {
                        return false;
                    }
                    saw_frame = true;
                } else if marker == 0xDA {
                    let components = usize::from(*data.get(segment_start).unwrap_or(&0));
                    if !saw_frame || segment_end < segment_start + 1 + (components * 2) + 3 {
                        return false;
                    }
                    saw_scan = true;
                    let Some(next_marker_offset) = find_next_jpeg_marker(data, segment_end) else {
                        return false;
                    };
                    if next_marker_offset == segment_end {
                        return false;
                    }
                    offset = next_marker_offset;
                    continue;
                }

                offset = segment_end;
            }
        }
    }

    false
}

pub(crate) fn detect_image_upload_content_type(data: &[u8]) -> Option<&'static str> {
    if is_valid_png(data) {
        return Some("image/png");
    }
    if is_valid_jpeg(data) {
        return Some("image/jpeg");
    }

    None
}

pub(crate) const MAX_IMAGE_UPLOAD_FILES: usize = 10;
pub(crate) const MAX_IMAGE_UPLOAD_BYTES: usize = 10 * 1024 * 1024;
pub(crate) const MAX_IMAGE_UPLOAD_REQUEST_BYTES: usize =
    MAX_IMAGE_UPLOAD_FILES * MAX_IMAGE_UPLOAD_BYTES + (1024 * 1024);

#[cfg(test)]
#[path = "tests/image_uploads_tests.rs"]
mod tests;
