    // Real TCP upstream fixture: each request is counted; EOF/truncation never replays.
    fn remediation_exchange(entry: &str, protocol: &str, status: u16, body: Vec<u8>, declared: usize, sse: bool) -> String {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = upstream.local_addr().unwrap();
        let worker = thread::spawn(move || {
            // Error statuses may use the pre-body transient retry (never body replay).
            upstream.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut count = 0;
            while Instant::now() < deadline {
                let (mut socket, _) = match upstream.accept() {
                    Ok(pair) => pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => { thread::sleep(Duration::from_millis(5)); continue; },
                    Err(error) => panic!("{error}"),
                };
                count += 1;
                let _ = read_request(&mut socket).unwrap();
                let ct = if sse { "text/event-stream" } else { "application/json" };
                write!(socket,"HTTP/1.1 {status} Test\r\ncontent-type: {ct}\r\ncontent-length: {declared}\r\nconnection: close\r\n\r\n").unwrap();
                let _ = socket.write_all(&body);
                if status == 200 { break; }
            }
            count
        });
        let mut b = binding("remediation", protocol, "local-fixture-token");
        b.entry_protocol = entry.to_string();
        b.base_url = format!("http://{address}");
        b.api_key = "FAKE_BEARER_unpatterned_secret".into();
        let mut gateway = InferenceGateway::start(Some(0), Box::new(move || vec![b.clone()]), false).unwrap();
        let path = match entry { "anthropic" => "/v1/messages", "openai-responses" => "/v1/responses", _ => "/v1/chat/completions" };
        let request = json!({"model":"model-a","messages":[{"role":"user","content":"hi"}],"input":"hi","max_tokens":8,"stream":sse}).to_string();
        let mut client = TcpStream::connect(("127.0.0.1", gateway.port())).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(client,"POST {path} HTTP/1.1\r\nhost: localhost\r\nauthorization: Bearer local-fixture-token\r\nx-api-key: local-fixture-token\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{request}",request.len()).unwrap();
        let mut result=String::new(); client.read_to_string(&mut result).unwrap();
        gateway.stop();
        let count=worker.join().unwrap();
        if status == 200 { assert_eq!(count,1,"body failure must not replay"); }
        result
    }
    #[test]
    fn remediation_error_matrix_real_loopback() {
        for (entry, protocol) in [("openai-chat","openai-chat"),("openai-responses","openai-responses"),("anthropic","anthropic"),("anthropic","openai-chat"),("anthropic","openai-responses"),("openai-responses","openai-chat"),("openai-chat","openai-responses")] {
            for status in [401,403,500] {
                let body=format!("<html>Bearer FAKE_BEARER_unpatterned_secret x-api-key FAKE_XAPI_unpatterned_secret {}</html>","x".repeat(ERROR_BODY_LIMIT+10)).into_bytes();
                let result=remediation_exchange(entry,protocol,status,body.clone(),body.len(),false);
                assert!(!result.contains("FAKE_"),"{entry}/{protocol}/{status}");
                assert!(result.contains("request_id"),"{result}");
                assert!(result.contains("himind_gateway_"),"{result}");
                assert!(result.len()<4096,"public error must be bounded");
            }
        }
    }
    #[test]
    fn remediation_body_truncated_and_complete_real_loopback() {
        let raw=br#"{"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#.to_vec();
        let result=remediation_exchange("openai-chat","openai-chat",200,raw.clone(),raw.len()+40,false);
        assert!(result.starts_with("HTTP/1.1 502"),"{result}");
        assert!(result.contains("truncated") || result.contains("upstream_io"),"{result}");
        let result=remediation_exchange("openai-chat","openai-chat",200,raw.clone(),raw.len(),false);
        assert!(result.starts_with("HTTP/1.1 200"),"{result}");
    }
    #[test]
    fn remediation_stream_boundaries_real_loopback() {
        for (entry, protocol) in [("openai-chat","openai-chat"),("anthropic","openai-chat"),("openai-responses","openai-chat"),("anthropic","openai-responses"),("openai-chat","openai-responses")] {
            let first=if protocol=="openai-chat" { "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1}}\n\n" } else { "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r\",\"output\":[]}}\n\n" };
            let result=remediation_exchange(entry,protocol,200,first.as_bytes().to_vec(),first.len()+100,true);
            assert!(result.contains("event: error"),"{entry}/{protocol}: {result}");
            assert!(!result.contains("response.completed") && !result.contains("message_stop"),"must not synthesize completion: {result}");
            let result=remediation_exchange(entry,protocol,200,b"data: {".to_vec(),100,true);
            assert!(result.starts_with("HTTP/1.1 502"),"{result}");
        }
    }
    #[test]
    fn remediation_body_limit_and_timeout_real_loopback() {
        let upstream=TcpListener::bind("127.0.0.1:0").unwrap(); let addr=upstream.local_addr().unwrap();
        let worker=thread::spawn(move || { let (mut s,_) = upstream.accept().unwrap(); let _=read_request(&mut s); write!(s,"HTTP/1.1 200 OK\r\ncontent-length: 100\r\n\r\n").unwrap(); thread::sleep(Duration::from_millis(200)); });
        let response=reqwest::blocking::Client::builder().no_proxy().timeout(Duration::from_millis(80)).build().unwrap().get(format!("http://{addr}")).send().unwrap();
        assert_eq!(read_success_body(response).unwrap_err(),BodyFailure::Timeout); worker.join().unwrap();
        let raw=vec![b' ';RESPONSE_BODY_LIMIT+1];
        let result=remediation_exchange("openai-chat","openai-chat",200,raw.clone(),raw.len(),false);
        assert!(result.contains("response_too_large"),"{result}");
        assert_eq!(classify_read_error(&std::io::Error::new(std::io::ErrorKind::ConnectionReset,"fake secret")),BodyFailure::Io);
    }
