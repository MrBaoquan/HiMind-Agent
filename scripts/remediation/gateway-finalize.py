from pathlib import Path
p=Path('src/app/inference_gateway.rs'); s=p.read_text(encoding='utf-8')
# Account for downstream body writes in all buffered translation branches.
a=s.index('/// Anthropic 入口 → OpenAI 上游'); b=s.index('fn translate_error(',a)
part=s[a:b]
for name in ['forward_translated','forward_anthropic_responses_translated','forward_responses_translated','forward_chat_responses_translated']:
    x=part.index('fn '+name+'(')
    y=part.find('\nfn ',x+3)
    if y<0:y=len(part)
    block=part[x:y]
    start=block.index('    let raw = match read_success_body(response)')
    prefix=block[:start]; tail=block[start:]
    tail=tail.replace('    if streaming {','    let mut delivered = true;\n    if streaming {',1) if name=='forward_anthropic_responses_translated' else tail
    tail=tail.replace('        let _ = stream.write_all(payload.as_bytes());\n        let _ = stream.flush();','        delivered &= stream.write_all(payload.as_bytes()).and_then(|_| stream.flush()).is_ok();')
    old='    let _ = stream.write_all(head.as_bytes());\n    let _ = stream.write_all(payload.as_bytes());\n    let _ = stream.flush();'
    tail=tail.replace(old,'    let delivered = stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(payload.as_bytes())).and_then(|_| stream.flush()).is_ok();')
    tail=tail.replace('        let _ = stream.write_all(head.as_bytes());\n        delivered &=','        delivered &= stream.write_all(head.as_bytes()).is_ok();\n        delivered &=')
    tail=tail.replace('    record_usage(','    if !delivered { log_transport_outcome("himind_gateway_client_io", true); }\n    record_usage(') if False else tail
    # only successful final accounting; failure arms retain their existing behavior
    pos=tail.rfind('    record_usage(')
    tail=tail[:pos]+'    if !delivered { log_transport_outcome("himind_gateway_client_io", true); }\n'+tail[pos:]
    pos=tail.rfind('    record_usage(')
    tail=tail[:pos]+tail[pos:].replace('        status.as_u16(),','        if delivered { status.as_u16() } else { 502 },',1).replace('        usage.is_empty(),','        !delivered || usage.is_empty(),',1)
    part=part[:x]+prefix+tail+part[y:]
s=s[:a]+part+s[b:]
# Test-only timeout knob; production deadline stays unchanged.
s=s.replace('fn upstream_client(base_url: &str)', '#[cfg(test)]\nstatic TEST_UPSTREAM_TIMEOUT_MS: AtomicU64 = AtomicU64::new(0);\n\nfn upstream_client(base_url: &str)')
s=s.replace('    let loopback = url::Url::parse(base_url)','    #[cfg(test)]\n    if TEST_UPSTREAM_TIMEOUT_MS.load(Ordering::Relaxed) > 0 {\n        builder = builder.timeout(Duration::from_millis(TEST_UPSTREAM_TIMEOUT_MS.load(Ordering::Relaxed)));\n    }\n    let loopback = url::Url::parse(base_url)')
s=s.replace('                let _ = socket.write_all(&body);','                if body == b"TIMEOUT" { thread::sleep(Duration::from_millis(250)); }\n                else { let _ = socket.write_all(&body); }')
s=s.replace('        let raw = vec![b\' \'; RESPONSE_BODY_LIMIT + 1];','''        TEST_UPSTREAM_TIMEOUT_MS.store(80, Ordering::Relaxed);
        let result = remediation_exchange("openai-chat", "openai-chat", 200, b"TIMEOUT".to_vec(), 100, false);
        TEST_UPSTREAM_TIMEOUT_MS.store(0, Ordering::Relaxed);
        assert!(result.starts_with("HTTP/1.1 502") && result.contains("upstream_timeout"), "{result}");
        let raw = vec![b' '; RESPONSE_BODY_LIMIT + 1];''')
p.write_text(s,encoding='utf-8')
