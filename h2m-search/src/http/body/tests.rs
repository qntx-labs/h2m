use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

#[tokio::test]
async fn known_length_accepts_boundary_and_rejects_overflow() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"12345678"))
        .mount(&server)
        .await;
    let http = HttpConfig::new().unwrap().with_max_response_bytes(8);
    let response = http.client().get(server.uri()).send().await.unwrap();
    assert_eq!(http.read_body("test", response).await.unwrap(), b"12345678");

    let http = http.with_max_response_bytes(7);
    let oversized_response = http.client().get(server.uri()).send().await.unwrap();
    let error = http
        .read_body("test", oversized_response)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        SearchError::ResponseTooLarge { limit_bytes: 7, .. }
    ));
    assert!(!error.is_retryable());
    assert_eq!(error.provider(), Some("test"));
    let json = serde_json::to_value(error).unwrap();
    assert_eq!(
        json.get("kind").and_then(serde_json::Value::as_str),
        Some("responseTooLarge")
    );
    assert_eq!(
        json.get("limitBytes").and_then(serde_json::Value::as_u64),
        Some(7)
    );
}

fn serve_chunked(listener: &TcpListener) {
    let (mut socket, _) = listener.accept().unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut request = Vec::new();
    while !request.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        socket.read_exact(&mut byte).unwrap();
        request.extend_from_slice(&byte);
    }
    socket.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\n1234\r\n4\r\n5678\r\n0\r\n\r\n").unwrap();
}

#[tokio::test]
async fn chunked_bodies_are_bounded_without_content_length() {
    for limit in [7, 8] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || serve_chunked(&listener));
        let http = HttpConfig::new().unwrap().with_max_response_bytes(limit);
        let response = http
            .client()
            .get(format!("http://{address}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.content_length(), None);
        let result = http.read_body("test", response).await;
        server.join().unwrap();
        if limit == 8 {
            assert_eq!(result.unwrap(), b"12345678");
        } else {
            assert!(matches!(
                result,
                Err(SearchError::ResponseTooLarge { limit_bytes: 7, .. })
            ));
        }
    }
}

#[cfg(any(
    feature = "wikipedia",
    feature = "searxng",
    feature = "brave",
    feature = "tavily"
))]
#[tokio::test]
async fn json_checks_size_before_parsing_and_preserves_parse_errors() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"not json"))
        .mount(&server)
        .await;
    for limit in [7, 8] {
        let http = HttpConfig::new().unwrap().with_max_response_bytes(limit);
        let response = http.client().get(server.uri()).send().await.unwrap();
        let error = http
            .read_json::<serde_json::Value>("test", response)
            .await
            .unwrap_err();
        if limit == 7 {
            assert!(matches!(error, SearchError::ResponseTooLarge { .. }));
        } else {
            assert!(matches!(error, SearchError::InvalidResponse { .. }));
        }
    }
}
