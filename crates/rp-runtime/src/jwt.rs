//! JWT（HS256）签发与校验。
//!
//! 只实现 HMAC-SHA256：这是服务端最常见、也是唯一不需要分发公钥的算法。
//! 校验时强制 `alg == HS256`，拒绝 `none` 与其它算法，避免算法混淆攻击。

use base64::Engine as _;
use hmac::Mac as _;

const HEADER: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";

/// 签发令牌：`payload` 必须是 JSON 对象；`ttl_secs > 0` 时写入 `exp`。
pub fn sign(payload: &serde_json::Value, secret: &str, ttl_secs: i64) -> Result<String, String> {
    if !payload.is_object() {
        return Err("jwt.sign expects a map payload".to_string());
    }
    let now = now_secs();
    let mut claims = payload.clone();
    if let Some(object) = claims.as_object_mut() {
        object
            .entry("iat")
            .or_insert_with(|| serde_json::Value::from(now));
        if ttl_secs > 0 {
            object.insert("exp".to_string(), serde_json::Value::from(now + ttl_secs));
        }
    }
    let body = serde_json::to_string(&claims).map_err(|error| error.to_string())?;
    let unsigned = format!("{}.{}", HEADER, base64_url(body.as_bytes()));
    let signature = base64_url(&hmac(secret, unsigned.as_bytes()));
    Ok(format!("{}.{}", unsigned, signature))
}

/// 带受众/签发者校验的变体：`aud`/`iss` 为 `Some` 时强制匹配
/// （`aud` 允许字符串或字符串数组，命中其一即可），防止同一密钥下不同服务/租户令牌互用。
/// 签名不符、过期或格式错误都返回 `Err`。
pub fn verify_claims(
    token: &str,
    secret: &str,
    expected_aud: Option<&str>,
    expected_iss: Option<&str>,
) -> Result<serde_json::Value, String> {
    let mut parts = token.split('.');
    let header = parts.next().unwrap_or_default();
    let payload = parts.next().unwrap_or_default();
    let signature = parts.next().unwrap_or_default();
    if header.is_empty() || payload.is_empty() || signature.is_empty() || parts.next().is_some() {
        return Err("malformed token".to_string());
    }
    let header_bytes = base64_url_decode(header).ok_or_else(|| "malformed header".to_string())?;
    let header_json: serde_json::Value =
        serde_json::from_slice(&header_bytes).map_err(|_| "malformed header".to_string())?;
    // 只接受 HS256：`none` 与其它算法一律拒绝（算法混淆防护）。
    if header_json.get("alg").and_then(|alg| alg.as_str()) != Some("HS256") {
        return Err("unsupported algorithm".to_string());
    }
    let unsigned = format!("{}.{}", header, payload);
    let expected = hmac(secret, unsigned.as_bytes());
    let provided = base64_url_decode(signature).ok_or_else(|| "malformed signature".to_string())?;
    if !constant_time_eq(&provided, &expected) {
        return Err("invalid signature".to_string());
    }
    let payload_bytes =
        base64_url_decode(payload).ok_or_else(|| "malformed payload".to_string())?;
    let claims: serde_json::Value =
        serde_json::from_slice(&payload_bytes).map_err(|_| "malformed payload".to_string())?;
    let now = now_secs();
    if let Some(exp) = claims.get("exp").and_then(|value| value.as_i64()) {
        if exp <= now {
            return Err("token expired".to_string());
        }
    }
    if let Some(nbf) = claims.get("nbf").and_then(|value| value.as_i64()) {
        if nbf > now {
            return Err("token not yet valid".to_string());
        }
    }
    if let Some(aud) = expected_aud {
        let matched = match claims.get("aud") {
            Some(serde_json::Value::String(value)) => value == aud,
            Some(serde_json::Value::Array(items)) => {
                items.iter().any(|item| item.as_str() == Some(aud))
            }
            _ => false,
        };
        if !matched {
            return Err("token audience mismatch".to_string());
        }
    }
    if let Some(iss) = expected_iss {
        if claims.get("iss").and_then(|value| value.as_str()) != Some(iss) {
            return Err("token issuer mismatch".to_string());
        }
    }
    Ok(claims)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs() as i64)
        .unwrap_or(0)
}

fn hmac(secret: &str, data: &[u8]) -> Vec<u8> {
    let mut mac = <hmac::Hmac<sha2::Sha256> as hmac::Mac>::new_from_slice(secret.as_bytes())
        .expect("hmac accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn base64_url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn base64_url_decode(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text.as_bytes())
        .ok()
}

/// 定长比较：避免通过响应时间推断签名前缀。
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verify(token: &str, secret: &str) -> Result<serde_json::Value, String> {
        verify_claims(token, secret, None, None)
    }

    #[test]
    fn signs_and_verifies() {
        let payload = serde_json::json!({"sub": "42", "role": "admin"});
        let token = sign(&payload, "secret", 60).expect("sign");
        let claims = verify(&token, "secret").expect("verify");
        assert_eq!(claims["sub"], "42");
        assert!(claims["exp"].as_i64().unwrap() > now_secs());
    }

    #[test]
    fn checks_audience_and_issuer() {
        let token = sign(
            &serde_json::json!({"sub": "1", "aud": "web", "iss": "auth.rrip"}),
            "secret",
            60,
        )
        .expect("sign");
        assert!(verify_claims(&token, "secret", Some("web"), Some("auth.rrip")).is_ok());
        assert_eq!(
            verify_claims(&token, "secret", Some("api"), None).unwrap_err(),
            "token audience mismatch"
        );
        assert_eq!(
            verify_claims(&token, "secret", None, Some("other")).unwrap_err(),
            "token issuer mismatch"
        );
        // 数组 aud 命中其一即可
        let multi = sign(
            &serde_json::json!({"aud": ["web", "mobile"]}),
            "secret",
            60,
        )
        .expect("sign");
        assert!(verify_claims(&multi, "secret", Some("mobile"), None).is_ok());
        assert!(verify_claims(&multi, "secret", Some("tv"), None).is_err());
    }

    #[test]
    fn rejects_tampered_signature() {
        let token = sign(&serde_json::json!({"a": 1}), "secret", 60).expect("sign");
        let mut tampered = token.clone();
        tampered.push('x');
        assert!(verify(&tampered, "secret").is_err());
        assert!(verify(&token, "other").is_err());
    }

    #[test]
    fn rejects_expired_and_none_alg() {
        let expired = sign(&serde_json::json!({"exp": now_secs() - 10}), "secret", 0)
            .expect("sign");
        assert_eq!(verify(&expired, "secret").unwrap_err(), "token expired");
        let none = format!(
            "{}.{}.{}",
            base64_url(br#"{"alg":"none","typ":"JWT"}"#),
            base64_url(br#"{"sub":"1"}"#),
            ""
        );
        assert_eq!(verify(&none, "secret").unwrap_err(), "malformed token");
    }
}
