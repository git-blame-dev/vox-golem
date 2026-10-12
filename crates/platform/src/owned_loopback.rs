use reqwest::{redirect, ClientBuilder};

/// Authenticated, process-owned loopback servers must not use ambient proxies or redirects.
/// Callers retain their TLS configuration, protocol, connect timeout and pool lifetime policy.
pub(crate) fn client_builder() -> ClientBuilder {
    owned_policy(reqwest::Client::builder())
}

fn owned_policy(builder: ClientBuilder) -> ClientBuilder {
    builder.no_proxy().redirect(redirect::Policy::none())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_request_headers(socket: &mut TcpStream) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let mut bytes = [0_u8; 4096];
        let mut used = 0;
        loop {
            let read = tokio::time::timeout_at(deadline, socket.read(&mut bytes[used..]))
                .await
                .expect("synthetic request header deadline")
                .unwrap();
            assert!(read > 0, "synthetic request ended before headers");
            used += read;
            if bytes[..used].windows(4).any(|part| part == b"\r\n\r\n") {
                return;
            }
            assert!(used < bytes.len(), "synthetic request header bound");
        }
    }

    fn with_test_tls(builder: ClientBuilder) -> ClientBuilder {
        #[cfg(feature = "custom-openai-webpki")]
        {
            // rustls-no-provider needs an explicitly selected provider even for
            // HTTP clients. These fixtures use only HTTP; no trust roots are needed.
            let tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .expect("test TLS protocol versions")
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
            builder.use_preconfigured_tls(tls)
        }
        #[cfg(not(feature = "custom-openai-webpki"))]
        {
            builder
        }
    }

    #[tokio::test]
    async fn owned_client_does_not_follow_authenticated_redirect() {
        let source = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let target = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = source.local_addr().unwrap();
        let destination = target.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = source.accept().await.unwrap();
            read_request_headers(&mut socket).await;
            socket.write_all(format!("HTTP/1.1 302 Found\r\nLocation: http://{destination}/synthetic\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        });
        let response = with_test_tls(client_builder())
            .build()
            .unwrap()
            .get(format!("http://{address}"))
            .bearer_auth("synthetic-owned-token")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), target.accept())
                .await
                .is_err()
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn owned_client_ignores_a_configured_proxy() {
        let proxy = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let source = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = source.local_addr().unwrap();
        // Explicit proxy injection avoids process-environment mutation and real network access.
        let builder = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(format!("http://{}", proxy.local_addr().unwrap())).unwrap());
        let client = with_test_tls(owned_policy(builder)).build().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = source.accept().await.unwrap();
            read_request_headers(&mut socket).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });
        assert!(client
            .get(format!("http://{address}"))
            .send()
            .await
            .unwrap()
            .status()
            .is_success());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), proxy.accept())
                .await
                .is_err()
        );
        server.await.unwrap();
    }
}
