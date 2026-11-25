//! Domain-neutral, declaration-scoped credential intake. Raw documents are never
//! sent to a model, capability ledger, plugin handler or error response.
use crate::capability::plugin::{PluginCredentialRequest, PluginRegistryItem};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::io::Read;
use std::time::Duration;

const LIMIT: usize = 64 * 1024;
pub(crate) const IMPORT_PERMISSION: &str = "secret.import.structured";
pub(crate) const NETWORK_PERMISSION: &str = "network.credential-import.endpoint";
const PORTABLE_PROTOCOL: &str = "himind.credential-import.v1";

/// Exact JSON discriminator, explicit input fields, and one read-only identity
/// probe. Business interpretation of its response belongs to the extension.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportDeclaration {
    pub schema: String,
    pub fields: BTreeMap<String, String>,
    pub optional_fields: Vec<String>,
    pub secret_prefixes: Vec<String>,
    pub endpoint_field: String,
    pub bearer_field: String,
    pub verification_path: String,
    pub handler: String,
}

pub(crate) fn validate_declaration(c: &PluginCredentialRequest) -> Result<(), Box<dyn Error>> {
    let Some(d) = &c.secure_import else {
        return Ok(());
    };
    if !d.schema.starts_with(&format!("{}.", c.id))
        || d.schema.len() > 128
        || !d
            .schema
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || d.handler != "credential.import.verify"
        || !d.verification_path.starts_with('/')
        || d.verification_path.starts_with("//")
        || d.verification_path.contains(['?', '#', '\\'])
        || d.verification_path.len() > 256
        || d.secret_prefixes.is_empty()
        || d.secret_prefixes.len() > 4
        || d.secret_prefixes.iter().any(|p| {
            p.len() < 8
                || p.len() > 64
                || !p
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        })
        || c.broker_capabilities.is_empty()
        || c.fields.len() > 8
        || d.fields.len() > 8
        || d.optional_fields.len() > 8
    {
        return Err("invalid structured credential import declaration".into());
    }
    let fields: BTreeMap<_, _> = c
        .fields
        .iter()
        .map(|f| (f.key.as_str(), f.secret))
        .collect();
    if fields.get(d.endpoint_field.as_str()) != Some(&false)
        || fields.get(d.bearer_field.as_str()) != Some(&true)
        || !d.fields.values().any(|v| v == &d.endpoint_field)
        || !d.fields.values().any(|v| v == &d.bearer_field)
        || d.fields
            .iter()
            .any(|(k, v)| k == "schema" || !fields.contains_key(v.as_str()))
        || d.optional_fields.iter().any(|k| !d.fields.contains_key(k))
    {
        return Err("invalid credential import field mapping".into());
    }
    Ok(())
}

pub(crate) fn declarations(
) -> Result<Vec<(PluginRegistryItem, PluginCredentialRequest)>, Box<dyn Error>> {
    let mut entries = Vec::new();
    // Disabled handlers still contribute deny markers, but never receive input.
    for plugin in crate::capability::plugin::scan_plugins()? {
        for c in &plugin.credentials {
            if c.secure_import.is_some() && validate_declaration(c).is_ok() {
                entries.push((plugin.clone(), c.clone()));
            }
        }
    }
    Ok(entries)
}

pub(crate) fn contains_sensitive(text: &str) -> bool {
    declarations()
        .map(|entries| sensitive_with(text, &entries))
        .unwrap_or(true)
}

fn sensitive_with(text: &str, entries: &[(PluginRegistryItem, PluginCredentialRequest)]) -> bool {
    fn literal(text: &str, entries: &[(PluginRegistryItem, PluginCredentialRequest)]) -> bool {
        text.contains(PORTABLE_PROTOCOL)
            || entries.iter().any(|(_, c)| {
                c.secure_import.as_ref().is_some_and(|d| {
                    text.contains(&d.schema) || d.secret_prefixes.iter().any(|p| text.contains(p))
                })
            })
    }
    if literal(text, entries) {
        return true;
    }
    // Match JSON Unicode escapes even when the envelope is surrounded by prose.
    // This is bounded preclassification, not arbitrary encoded-secret detection.
    let bytes = text.as_bytes();
    let mut decoded = String::with_capacity(text.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor..].starts_with(b"\\u")
            && cursor + 6 <= bytes.len()
            && bytes[cursor + 2..cursor + 6]
                .iter()
                .all(u8::is_ascii_hexdigit)
        {
            if let Ok(value) = u32::from_str_radix(&text[cursor + 2..cursor + 6], 16) {
                if let Some(character) = char::from_u32(value) {
                    decoded.push(character);
                    cursor += 6;
                    continue;
                }
            }
        }
        let character = text[cursor..].chars().next().expect("valid UTF-8 boundary");
        decoded.push(character);
        cursor += character.len_utf8();
    }
    literal(&decoded, entries)
}

pub(crate) fn contains_secret_body(body: &[u8]) -> bool {
    fn visit(v: &Value, entries: &[(PluginRegistryItem, PluginCredentialRequest)]) -> bool {
        match v {
            Value::String(s) => sensitive_with(s, entries),
            Value::Array(a) => a.iter().any(|v| visit(v, entries)),
            Value::Object(o) => o
                .iter()
                .any(|(k, v)| sensitive_with(k, entries) || visit(v, entries)),
            _ => false,
        }
    }
    let Ok(entries) = declarations() else {
        return true;
    };
    serde_json::from_slice(body)
        .map(|v| visit(&v, &entries))
        .unwrap_or_else(|_| sensitive_with(&String::from_utf8_lossy(body), &entries))
}

/// This endpoint returns only non-secret markers. Clients need no business rules.
pub(crate) fn matchers() -> Result<Value, Box<dyn Error>> {
    Ok(
        json!({"schemas": declarations()?.iter().filter_map(|(_,c)| c.secure_import.as_ref()).map(|d|
        json!({"schema":d.schema,"secret_prefixes":d.secret_prefixes})).collect::<Vec<_>>()}),
    )
}

pub(crate) fn preprocess(text: &str) -> Result<Value, Box<dyn Error>> {
    if text.len() > LIMIT {
        return Err("secure message exceeds size limit".into());
    }
    let entries = declarations()?;
    let mut cursor = 0;
    let mut output = String::new();
    let mut receipts = Vec::new();
    for (start, _) in text.match_indices('{') {
        if start < cursor {
            continue;
        }
        let mut stream = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        let Some(Ok(value)) = stream.next() else {
            continue;
        };
        let Some(schema) = value.get("schema").and_then(Value::as_str) else {
            continue;
        };
        let extension = value.get("extensionId").and_then(Value::as_str);
        let handlers: Vec<_> = entries
            .iter()
            .filter(|(plugin, c)| {
                extension.is_none_or(|id| id == plugin.id)
                    && c.secure_import.as_ref().is_some_and(|d| d.schema == schema)
            })
            .collect();
        if handlers.is_empty() {
            if value.get("importProtocol").and_then(Value::as_str) == Some(PORTABLE_PROTOCOL) {
                return Err("required credential extension is not installed".into());
            }
            continue;
        }
        // Ambiguous claiming is a denial, never first-plugin-wins dispatch.
        if handlers.len() != 1 {
            return Err("ambiguous secure import handler".into());
        }
        let (plugin, credential) = handlers[0];
        let receipt = import_document(plugin, credential, &value)
            .map_err(|_| "credential import failed; original message was not saved")?;
        output.push_str(&text[cursor..start]);
        output.push_str(&format!("[凭证已在本机安全导入；extension={}; credential_ref={}; 请使用扩展的授权目录和工程绑定能力。]", plugin.id, receipt["credential_ref"].as_str().unwrap_or_default()));
        receipts.push(receipt);
        cursor = start + stream.byte_offset();
    }
    output.push_str(&text[cursor..]);
    if sensitive_with(&output, &entries) {
        return Err("unprocessed credential document; message blocked".into());
    }
    Ok(json!({"content":output,"imports":receipts}))
}

fn origin(value: &str) -> Result<String, Box<dyn Error>> {
    let u = reqwest::Url::parse(value)?;
    let local = matches!(
        u.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    );
    if u.host_str().is_none()
        || !(u.scheme() == "https" || u.scheme() == "http" && local)
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
        || u.path() != "/"
    {
        return Err("invalid credential endpoint origin".into());
    }
    Ok(u.as_str().trim_end_matches('/').to_string())
}

fn import_document(
    plugin: &PluginRegistryItem,
    c: &PluginCredentialRequest,
    doc: &Value,
) -> Result<Value, Box<dyn Error>> {
    import_document_with(plugin, c, doc, &|input| {
        crate::capability::plugin::invoke_credential_import_handler(plugin, input)
    })
}

fn import_document_with(
    plugin: &PluginRegistryItem,
    c: &PluginCredentialRequest,
    doc: &Value,
    handler: &dyn Fn(Value) -> Result<Value, Box<dyn Error>>,
) -> Result<Value, Box<dyn Error>> {
    if !plugin.enabled
        || plugin.status != "installed"
        || ![
            IMPORT_PERMISSION,
            NETWORK_PERMISSION,
            "secret.broker.acquire",
        ]
        .iter()
        .all(|p| plugin.permissions.iter().any(|v| v == p))
    {
        return Err("installed authorized credential handler required".into());
    }
    let d = c.secure_import.as_ref().ok_or("missing declaration")?;
    if doc
        .get("extensionId")
        .is_some_and(|v| v.as_str() != Some(&plugin.id))
        || doc
            .get("importProtocol")
            .is_some_and(|v| v.as_str() != Some(PORTABLE_PROTOCOL))
    {
        return Err("credential document extension identity mismatch".into());
    }
    let object = doc.as_object().ok_or("invalid document")?;
    if object.keys().any(|key| {
        !matches!(key.as_str(), "schema" | "extensionId" | "importProtocol")
            && !d.fields.contains_key(key)
    }) {
        return Err("undeclared import field".into());
    }
    let mut public = BTreeMap::new();
    let mut secrets = BTreeMap::new();
    let mut safe_document = doc.clone();
    if let Some(object) = safe_document.as_object_mut() {
        object.remove("extensionId");
        object.remove("importProtocol");
    }
    for (from, to) in &d.fields {
        let field = c
            .fields
            .iter()
            .find(|f| &f.key == to)
            .ok_or("undeclared credential field")?;
        let Some(value) = doc.get(from).and_then(Value::as_str) else {
            if d.optional_fields.contains(from) {
                continue;
            }
            return Err("required import field missing".into());
        };
        if value.is_empty() || value.len() > 4096 {
            return Err("invalid import field size".into());
        }
        if field.secret {
            if !d
                .secret_prefixes
                .iter()
                .any(|prefix| value.starts_with(prefix))
            {
                return Err("secret does not match declaration".into());
            }
            secrets.insert(to.clone(), value.to_string());
            safe_document
                .as_object_mut()
                .ok_or("invalid document")?
                .remove(from);
        } else {
            public.insert(to.clone(), value.to_string());
        }
    }
    let base = origin(public.get(&d.endpoint_field).ok_or("endpoint missing")?)?;
    public.insert(d.endpoint_field.clone(), base.clone());
    let key = secrets
        .get(&d.bearer_field)
        .ok_or("bearer credential missing")?;
    let safe_text = safe_document.to_string();
    if d.secret_prefixes.iter().any(|p| safe_text.contains(p))
        || secrets.values().any(|s| safe_text.contains(s))
    {
        return Err("secret in public import fields".into());
    }
    // The extension declares this exact read-only probe; the host enforces
    // transport, origin, no redirects, body limit and generic errors.
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let response = client
        .get(format!("{base}{}", d.verification_path))
        .bearer_auth(key)
        .send()
        .map_err(|_| "credential endpoint unavailable")?;
    if !response.status().is_success() {
        return Err("credential endpoint denied identity probe".into());
    }
    let mut bytes = Vec::new();
    response.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err("identity response too large".into());
    }
    let identity: Value =
        serde_json::from_slice(&bytes).map_err(|_| "invalid identity response")?;
    let raw = identity.to_string();
    if secrets.values().any(|s| raw.contains(s))
        || d.secret_prefixes.iter().any(|p| raw.contains(p))
    {
        return Err("identity response exposed secret".into());
    }
    let verified = handler(json!({"credential_id":c.id,"document":safe_document,"public_fields":public,"response":identity}))
        .map_err(|_| "credential handler verification failed")?;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Verified {
        public_fields: BTreeMap<String, String>,
        identity: String,
    }
    let verified: Verified =
        serde_json::from_value(verified).map_err(|_| "invalid handler output")?;
    if verified.identity.is_empty() || verified.identity.len() > 256 {
        return Err("invalid handler identity".into());
    }
    let allowed: BTreeSet<_> = c
        .fields
        .iter()
        .filter(|f| !f.secret)
        .map(|f| f.key.as_str())
        .collect();
    if verified
        .public_fields
        .keys()
        .any(|k| !allowed.contains(k.as_str()))
        || c.fields
            .iter()
            .filter(|f| f.required && !f.secret)
            .any(|f| !verified.public_fields.contains_key(&f.key))
        || verified.public_fields.get(&d.endpoint_field) != Some(&base)
    {
        return Err("handler returned undeclared fields or changed origin".into());
    }
    let serialized = serde_json::to_string(&verified.public_fields)?;
    if serialized.len() > 16 * 1024
        || d.secret_prefixes
            .iter()
            .any(|p| serialized.contains(p) || verified.identity.contains(p))
        || secrets
            .values()
            .any(|s| serialized.contains(s) || verified.identity.contains(s))
    {
        return Err("handler output contains secret".into());
    }
    let reference = format!(
        "cred-{:x}",
        Sha256::digest(format!(
            "{}\n{}\n{}\n{}",
            plugin.id, c.id, base, verified.identity
        ))
    );
    let reference = &reference[..53];
    let fingerprint =
        crate::capability::plugin::credential_declaration_fingerprint(&plugin.credentials);
    crate::store::plugin_credentials::save_instance(
        &plugin.id,
        &c.id,
        reference,
        &verified.public_fields,
        &secrets,
        &fingerprint,
    )?;
    crate::store::plugin_credentials::record_audit(
        "plugin",
        &plugin.id,
        &c.id,
        "credential.import",
        "allowed",
        "",
    )?;
    Ok(
        json!({"schema":"himind.credential.receipt.v1","extension_id":plugin.id,"credential_id":c.id,"credential_ref":reference}),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{Read, Write};

    pub(crate) fn fixture(
        root: &std::path::Path,
        id: &str,
        prefix: &str,
    ) -> (PluginRegistryItem, PluginCredentialRequest) {
        let directory = root.join("plugins").join(id).join("current");
        std::fs::create_dir_all(&directory).unwrap();
        let manifest = json!({"id":id,"name":"Generic fixture","version":"1.0.0","entry":"fixture.exe","runtime":"process-jsonrpc-stdio","capability_namespace":"fixture","permissions":[IMPORT_PERMISSION,NETWORK_PERMISSION,"secret.broker.acquire"],"credentials":[{"id":"fixture.account","broker_capabilities":["fixture.target.bind","fixture.target.publish"],"fields":[{"key":"endpoint","secret":false,"required":true},{"key":"account","secret":false,"required":true},{"key":"token","secret":true,"required":true}],"secure_import":{"schema":format!("fixture.account.{}.v1",id.replace('.',"-")),"fields":{"endpoint":"endpoint","token":"token"},"optional_fields":[],"secret_prefixes":[prefix],"endpoint_field":"endpoint","bearer_field":"token","verification_path":"/identity","handler":"credential.import.verify"},"workspace_binding":{"root_field":"page_root","bind_capability":"fixture.target.bind","bound_capabilities":["fixture.target.publish"],"target_fields":["resource"]}}],"capabilities":[{"id":"fixture.target.bind"},{"id":"fixture.target.publish"}]});
        std::fs::write(directory.join("plugin.json"), manifest.to_string()).unwrap();
        std::fs::write(directory.join("fixture.exe"), b"not executed").unwrap();
        let plugin = crate::capability::plugin::find_plugin(id).unwrap().unwrap();
        let credential = plugin.credentials[0].clone();
        (plugin, credential)
    }

    #[test]
    fn exact_declarations_reject_broad_match_and_public_secret_mapping() {
        let _lock = crate::store::paths::test_env_lock();
        let root = std::env::temp_dir().join(format!(
            "generic-import-declaration-{}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let previous = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        let (_, mut c) = fixture(&root, "com.fixture.alpha", "fixture_alpha_");
        assert!(validate_declaration(&c).is_ok());
        c.secure_import.as_mut().unwrap().secret_prefixes = vec!["key".into()];
        assert!(validate_declaration(&c).is_err());
        c.secure_import.as_mut().unwrap().secret_prefixes = vec!["fixture_alpha_".into()];
        c.secure_import.as_mut().unwrap().schema = "*".into();
        assert!(validate_declaration(&c).is_err());
        match previous {
            Some(v) => std::env::set_var("HIMIND_AGENT_HOME", v),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        };
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn second_business_uses_same_import_pipeline_and_never_exposes_secret_to_handler() {
        let _lock = crate::store::paths::test_env_lock();
        let root = std::env::temp_dir().join(format!("generic-import-{}", rand::random::<u64>()));
        let previous = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        for (plugin_id, prefix) in [
            ("com.fixture.alpha", "fixture_alpha_"),
            ("com.fixture.beta", "fixture_beta_"),
        ] {
            let (plugin, c) = fixture(&root, plugin_id, prefix);
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let key = format!("{prefix}{}", "x".repeat(43));
            let expected = key.clone();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut bytes = [0u8; 4096];
                let n = stream.read(&mut bytes).unwrap();
                let text = String::from_utf8_lossy(&bytes[..n]);
                assert!(text.starts_with("GET /identity "));
                assert!(text.contains(&format!("Bearer {expected}")));
                let body = r#"{"identity":"account-7"}"#;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            });
            let doc = json!({"schema":c.secure_import.as_ref().unwrap().schema,"endpoint":endpoint,"token":key});
            let handler = |input: Value| -> Result<Value, Box<dyn Error>> {
                assert!(!input.to_string().contains(&key));
                assert_eq!(input["response"]["identity"], "account-7");
                Ok(
                    json!({"identity":"account-7","public_fields":{"endpoint":endpoint,"account":"account-7"}}),
                )
            };
            let receipt = import_document_with(&plugin, &c, &doc, &handler).unwrap();
            server.join().unwrap();
            assert!(!receipt.to_string().contains(&key));
            let reference = receipt["credential_ref"].as_str().unwrap();
            let fingerprint =
                crate::capability::plugin::credential_declaration_fingerprint(&plugin.credentials);
            assert_eq!(
                crate::store::plugin_credentials::broker_instance_fields(
                    plugin_id,
                    &c.id,
                    reference,
                    &fingerprint
                )
                .unwrap()["token"],
                key
            );
            assert!(
                !std::fs::read_to_string(root.join("plugin-credentials.json"))
                    .unwrap()
                    .contains(&key)
            );
            assert!(crate::store::plugin_credentials::broker_instance_fields(
                "com.fixture.other",
                &c.id,
                reference,
                &fingerprint
            )
            .is_err());
        }
        match previous {
            Some(v) => std::env::set_var("HIMIND_AGENT_HOME", v),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        };
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unavailable_extension_and_escaped_secret_fail_before_persistence() {
        let _lock = crate::store::paths::test_env_lock();
        let root =
            std::env::temp_dir().join(format!("generic-import-deny-{}", rand::random::<u64>()));
        let previous = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        assert!(preprocess(r#"{"importProtocol":"himind.credential-import.v1","schema":"unknown.v1","token":"private"}"#).is_err());
        fixture(&root, "com.fixture.alpha", "fixture_alpha_");
        assert!(contains_secret_body(
            br#"{"nested":["fixture\u005falpha_test"]}"#
        ));
        assert!(contains_secret_body(
            br#"{"fixture\u005falpha_test":"hidden in key"}"#
        ));
        assert!(preprocess(
            r#"说明 {"importProtocol":"himind.credential\u002dimport.v1","token":"private"} 后续"#
        )
        .is_err());
        assert!(preprocess("fixture_alpha_incomplete").is_err());
        assert_eq!(
            preprocess("ordinary message").unwrap()["content"],
            "ordinary message"
        );
        assert!(!root.join("plugin-credentials.json").exists());
        match previous {
            Some(v) => std::env::set_var("HIMIND_AGENT_HOME", v),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        };
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejected_identity_handler_cannot_persist_raw_document_or_secret() {
        let _lock = crate::store::paths::test_env_lock();
        let root =
            std::env::temp_dir().join(format!("generic-import-reject-{}", rand::random::<u64>()));
        let previous = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        let (plugin, c) = fixture(&root, "com.fixture.alpha", "fixture_alpha_");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = [0u8; 4096];
            let _ = stream.read(&mut bytes).unwrap();
            let body = r#"{"identity":"rejected"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let doc = json!({"schema":c.secure_import.as_ref().unwrap().schema,"endpoint":endpoint,"token":"fixture_alpha_private"});
        assert!(import_document_with(&plugin, &c, &doc, &|_| Err(
            "handler rejection must stay local".into()
        ))
        .is_err());
        server.join().unwrap();
        assert!(!root.join("plugin-credentials.json").exists());
        assert!(!root.join("credential-audit.jsonl").exists());
        match previous {
            Some(v) => std::env::set_var("HIMIND_AGENT_HOME", v),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        };
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "requires ANDX_IMPORT_PLUGIN_ROOT with the real built extension"]
    fn real_extension_import_and_binding_use_generic_host_protocol() {
        let _lock = crate::store::paths::test_env_lock();
        let source = std::path::PathBuf::from(
            std::env::var("ANDX_IMPORT_PLUGIN_ROOT").expect("extension root required"),
        );
        let root =
            std::env::temp_dir().join(format!("generic-real-extension-{}", rand::random::<u64>()));
        let previous = std::env::var_os("HIMIND_AGENT_HOME");
        std::env::set_var("HIMIND_AGENT_HOME", &root);
        let plugin_id = "com.himind.andx-custom-page-tools";
        let directory = root.join("plugins").join(plugin_id).join("current");
        std::fs::create_dir_all(directory.join("bin")).unwrap();
        std::fs::copy(source.join("plugin.json"), directory.join("plugin.json")).unwrap();
        std::fs::copy(
            source.join("bin/andx-custom-page-tools.exe"),
            directory.join("bin/andx-custom-page-tools.exe"),
        )
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let key = format!("andx_dev_{}", "x".repeat(43));
        let expected = key.clone();
        let server = std::thread::spawn(move || {
            for index in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut bytes = [0u8; 16384];
                let n = stream.read(&mut bytes).unwrap();
                let request = String::from_utf8_lossy(&bytes[..n]);
                assert!(request
                    .to_ascii_lowercase()
                    .contains(&format!("authorization: bearer {expected}")));
                let body = if index == 0 {
                    r#"{"code":0,"data":{"developer":{"id":"7","handle":"fixture"},"projects":[]}}"#
                } else {
                    assert!(
                        request.starts_with("GET /api/developer/projects/1/exhibits/11/context ")
                    );
                    r#"{"code":0,"data":{"project":{"id":"1"},"exhibit":{"id":"11"},"pageKey":"exhibit-11"}}"#
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let raw = format!(
            "请接入\n```json\n{}\n```",
            json!({"schema":"andx.developer.access.v1","importProtocol":"himind.credential-import.v1","extensionId":plugin_id,"endpoint":endpoint,"developer":"fixture","developerKey":key})
        );
        let receipt = preprocess(&raw).unwrap();
        assert!(!receipt.to_string().contains(&key));
        let reference = receipt["imports"][0]["credential_ref"].as_str().unwrap();
        let workspace = root.join("repo/page");
        std::fs::create_dir_all(&workspace).unwrap();
        let context = crate::capability::types::InvocationContext::new(
            crate::capability::types::InvocationSource::Mcp,
            "fixture",
        )
        .with_trusted_workspace(workspace.to_string_lossy().to_string());
        let mut input = json!({"project_root":workspace,"credential_ref":reference,"project_id":"1","exhibit_id":"11"});
        let prepared = crate::app::extension_binding::prepare(
            &context,
            "andx.page.workspace.bind",
            &mut input,
        )
        .unwrap()
        .unwrap();
        assert!(crate::capability::plugin::invoke_plugin_capability(
            "andx.page.workspace.bind",
            input.clone(),
            None,
            &[]
        )
        .is_err());
        {
            let _guard = prepared.execution_lock().unwrap();
            let env = vec![(
                "HIMIND_VERIFIED_BINDING".into(),
                crate::approval::policy::args_digest(&input).unwrap(),
            )];
            let mut output = crate::capability::plugin::invoke_plugin_capability(
                "andx.page.workspace.bind",
                input.clone(),
                None,
                &env,
            )
            .unwrap();
            prepared.complete(&input, &mut output).unwrap();
        }
        let mut args = json!({"project_root":workspace});
        let prepared = crate::app::extension_binding::prepare(
            &context,
            "andx.page.workspace.resolve",
            &mut args,
        )
        .unwrap()
        .unwrap();
        let _guard = prepared.execution_lock().unwrap();
        let env = vec![(
            "HIMIND_VERIFIED_BINDING".into(),
            crate::approval::policy::args_digest(&args).unwrap(),
        )];
        let output = crate::capability::plugin::invoke_plugin_capability(
            "andx.page.workspace.resolve",
            args.clone(),
            None,
            &env,
        )
        .unwrap();
        assert_eq!(output["exhibit_id"], "11");
        assert_eq!(output["page_key"], "exhibit-11");
        assert_eq!(output["binding_revision"], args["binding_revision"]);
        assert!(!output.to_string().contains(&key));
        server.join().unwrap();
        drop(_guard);
        match previous {
            Some(v) => std::env::set_var("HIMIND_AGENT_HOME", v),
            None => std::env::remove_var("HIMIND_AGENT_HOME"),
        };
        let _ = std::fs::remove_dir_all(root);
    }
}
