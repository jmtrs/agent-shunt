use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
};

use agent_shunt::{
    adapters::openai_compatible::OpenAiCompatibleWorker,
    application::ports::ContextWorker,
    domain::{Document, Limits, LineRange, WorkerRequest},
};

fn read_request(stream: &mut TcpStream) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let read = stream.read(&mut chunk).expect("read request");
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(header_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&buffer[..header_end]).to_ascii_lowercase();
            let content_length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if buffer.len() >= header_end + 4 + content_length {
                break;
            }
        }
        if read == 0 {
            break;
        }
    }
    String::from_utf8(buffer).expect("utf-8 request")
}

#[test]
fn worker_payload_contains_selected_chunks_not_full_document_lines() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub server");
    let port = listener.local_addr().expect("stub address").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept request");
        let request = read_request(&mut stream);
        let content = r#"{\"answer\":\"ok\",\"findings\":[],\"uncertainties\":[]}"#;
        let response = serde_json::json!({
            "choices": [{"message": {"content": content}}],
            "model": "stub-model"
        })
        .to_string();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            response.len(),
            response
        )
        .expect("write response");
        stream.flush().expect("flush response");
        request
    });

    let document = Document {
        path: "src/example.rs".to_owned(),
        bytes: 18,
        line_count: 2,
        lines: vec![
            "SELECTED_CHUNK_MARKER".to_owned(),
            "UNSELECTED_PRIVATE_MARKER".to_owned(),
        ],
        numbered_content: "1: SELECTED_CHUNK_MARKER".to_owned(),
        allowed_ranges: vec![LineRange {
            start_line: 1,
            end_line: 1,
        }],
    };
    let request = WorkerRequest {
        model: "stub-model".to_owned(),
        question: "inspect selected evidence".to_owned(),
        documents: vec![document],
        limits: Limits {
            timeout_ms: 5_000,
            ..Limits::default()
        },
        review: false,
    };
    let worker = OpenAiCompatibleWorker::new(&format!("http://127.0.0.1:{port}/v1"), "json_object");

    worker.analyze(&request, "").expect("worker request");
    let wire_request = server.join().expect("stub server");

    assert!(wire_request.contains("SELECTED_CHUNK_MARKER"));
    assert!(
        !wire_request.contains("UNSELECTED_PRIVATE_MARKER"),
        "the provider request must not contain source lines outside numbered_content"
    );
}
