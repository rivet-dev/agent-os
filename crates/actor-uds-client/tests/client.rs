use std::io;
use std::time::Duration;

use agentos_actor_uds_client::protocol as wire;
use agentos_actor_uds_client::{ActorUdsClient, ActorUdsClientConfig, ActorUdsError, SqlValue};
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Notify;
use vbare::OwnedVersionedData;

async fn read_frame(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let len = stream.read_u32().await?;
    let mut payload = vec![0; len as usize];
    stream.read_exact(&mut payload).await?;
    Ok(payload)
}

async fn write_frame(stream: &mut UnixStream, payload: &[u8]) -> io::Result<()> {
    stream.write_u32(payload.len() as u32).await?;
    stream.write_all(payload).await?;
    stream.flush().await
}

#[tokio::test]
async fn handshakes_and_reuses_a_connection_for_query_and_exec() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("actor.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        wire::versioned::ClientHello::deserialize_with_embedded_version(
            &read_frame(&mut stream).await.unwrap(),
        )
        .unwrap();
        let response =
            wire::versioned::ServerHello::wrap_latest(wire::ServerHello::HelloOk(wire::HelloOk {
                max_frame_bytes: 32 * 1024 * 1024,
            }))
            .serialize_with_embedded_version(1)
            .unwrap();
        write_frame(&mut stream, &response).await.unwrap();

        let request = wire::versioned::ClientFrame::deserialize_with_embedded_version(
            &read_frame(&mut stream).await.unwrap(),
        )
        .unwrap();
        let wire::ClientFrame::Request(request) = request;
        assert!(matches!(
            request.payload,
            wire::RequestPayload::SqliteQuery(_)
        ));
        let response = wire::versioned::ServerFrame::wrap_latest(wire::ServerFrame::Response(
            wire::Response {
                request_id: request.request_id,
                payload: wire::ResponsePayload::SqliteQueryOk(wire::SqliteQueryOk {
                    columns: vec!["value".to_owned()],
                    rows: vec![vec![wire::SqlValue::SqlInteger(42)]],
                    changes: 0,
                    last_insert_row_id: None,
                }),
            },
        ))
        .serialize_with_embedded_version(1)
        .unwrap();
        write_frame(&mut stream, &response).await.unwrap();

        let request = wire::versioned::ClientFrame::deserialize_with_embedded_version(
            &read_frame(&mut stream).await.unwrap(),
        )
        .unwrap();
        let wire::ClientFrame::Request(request) = request;
        assert!(matches!(
            request.payload,
            wire::RequestPayload::SqliteExec(_)
        ));
        let response = wire::versioned::ServerFrame::wrap_latest(wire::ServerFrame::Response(
            wire::Response {
                request_id: request.request_id,
                payload: wire::ResponsePayload::SqliteExecOk,
            },
        ))
        .serialize_with_embedded_version(1)
        .unwrap();
        write_frame(&mut stream, &response).await.unwrap();
    });

    let runtime =
        agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
            .unwrap();
    let client =
        ActorUdsClient::new(&path, runtime.context(), ActorUdsClientConfig::default()).unwrap();
    let result = client
        .query("SELECT ?", vec![SqlValue::SqlInteger(42)])
        .await
        .unwrap();
    assert_eq!(result.columns, ["value"]);
    assert_eq!(result.rows, [vec![SqlValue::SqlInteger(42)]]);
    client.exec("CREATE TABLE test (id INTEGER)").await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn reports_version_rejection_as_a_typed_error() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("actor.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_frame(&mut stream).await.unwrap();
        let response = wire::versioned::ServerHello::wrap_latest(
            wire::ServerHello::HelloRejectUnsupportedVersion,
        )
        .serialize_with_embedded_version(1)
        .unwrap();
        write_frame(&mut stream, &response).await.unwrap();
    });

    let runtime =
        agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
            .unwrap();
    let error = ActorUdsClient::new(&path, runtime.context(), ActorUdsClientConfig::default())
        .unwrap()
        .query("SELECT 1", Vec::new())
        .await
        .unwrap_err();
    assert!(matches!(error, ActorUdsError::VersionMismatch));
    server.await.unwrap();
}

#[tokio::test]
async fn routes_out_of_order_responses_to_concurrent_requests() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("actor.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_frame(&mut stream).await.unwrap();
        let hello =
            wire::versioned::ServerHello::wrap_latest(wire::ServerHello::HelloOk(wire::HelloOk {
                max_frame_bytes: 32 * 1024 * 1024,
            }))
            .serialize_with_embedded_version(1)
            .unwrap();
        write_frame(&mut stream, &hello).await.unwrap();

        let mut requests = Vec::new();
        for _ in 0..2 {
            let wire::ClientFrame::Request(request) =
                wire::versioned::ClientFrame::deserialize_with_embedded_version(
                    &read_frame(&mut stream).await.unwrap(),
                )
                .unwrap();
            requests.push(request);
        }
        for request in requests.into_iter().rev() {
            let wire::RequestPayload::SqliteQuery(query) = request.payload else {
                panic!("expected query request");
            };
            let value = if query.sql == "SELECT 1" { 1 } else { 2 };
            let response = wire::versioned::ServerFrame::wrap_latest(wire::ServerFrame::Response(
                wire::Response {
                    request_id: request.request_id,
                    payload: wire::ResponsePayload::SqliteQueryOk(wire::SqliteQueryOk {
                        columns: vec!["value".to_owned()],
                        rows: vec![vec![wire::SqlValue::SqlInteger(value)]],
                        changes: 0,
                        last_insert_row_id: None,
                    }),
                },
            ))
            .serialize_with_embedded_version(1)
            .unwrap();
            write_frame(&mut stream, &response).await.unwrap();
        }
    });

    let runtime =
        agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
            .unwrap();
    let client =
        ActorUdsClient::new(&path, runtime.context(), ActorUdsClientConfig::default()).unwrap();
    let first_client = client.clone();
    let first = tokio::spawn(async move { first_client.query("SELECT 1", Vec::new()).await });
    let second = tokio::spawn(async move { client.query("SELECT 2", Vec::new()).await });
    assert_eq!(
        first.await.unwrap().unwrap().rows,
        [vec![SqlValue::SqlInteger(1)]]
    );
    assert_eq!(
        second.await.unwrap().unwrap().rows,
        [vec![SqlValue::SqlInteger(2)]]
    );
    server.await.unwrap();
}

#[tokio::test]
async fn request_timeout_closes_the_generation_before_reconnect() {
    async fn handshake(stream: &mut UnixStream) {
        read_frame(stream).await.unwrap();
        let hello =
            wire::versioned::ServerHello::wrap_latest(wire::ServerHello::HelloOk(wire::HelloOk {
                max_frame_bytes: 32 * 1024 * 1024,
            }))
            .serialize_with_embedded_version(1)
            .unwrap();
        write_frame(stream, &hello).await.unwrap();
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("actor.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        handshake(&mut first).await;
        read_frame(&mut first).await.unwrap();

        let (mut second, _) = listener.accept().await.unwrap();
        handshake(&mut second).await;
        let wire::ClientFrame::Request(request) =
            wire::versioned::ClientFrame::deserialize_with_embedded_version(
                &read_frame(&mut second).await.unwrap(),
            )
            .unwrap();
        let response = wire::versioned::ServerFrame::wrap_latest(wire::ServerFrame::Response(
            wire::Response {
                request_id: request.request_id,
                payload: wire::ResponsePayload::SqliteQueryOk(wire::SqliteQueryOk {
                    columns: vec!["value".to_owned()],
                    rows: vec![vec![wire::SqlValue::SqlInteger(2)]],
                    changes: 0,
                    last_insert_row_id: None,
                }),
            },
        ))
        .serialize_with_embedded_version(1)
        .unwrap();
        write_frame(&mut second, &response).await.unwrap();
    });

    let runtime =
        agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
            .unwrap();
    let config = ActorUdsClientConfig {
        request_timeout: Duration::from_millis(50),
        ..ActorUdsClientConfig::default()
    };
    let client = ActorUdsClient::new(&path, runtime.context(), config).unwrap();
    assert!(matches!(
        client.query("SELECT 1", Vec::new()).await,
        Err(ActorUdsError::Timeout { .. })
    ));
    let result = client.query("SELECT 2", Vec::new()).await.unwrap();
    assert_eq!(result.rows, [vec![SqlValue::SqlInteger(2)]]);
    server.await.unwrap();
}

#[tokio::test]
async fn rejects_requests_above_the_in_flight_limit() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("actor.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let received = std::sync::Arc::new(Notify::new());
    let release = std::sync::Arc::new(Notify::new());
    let server_received = received.clone();
    let server_release = release.clone();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_frame(&mut stream).await.unwrap();
        let hello =
            wire::versioned::ServerHello::wrap_latest(wire::ServerHello::HelloOk(wire::HelloOk {
                max_frame_bytes: 32 * 1024 * 1024,
            }))
            .serialize_with_embedded_version(1)
            .unwrap();
        write_frame(&mut stream, &hello).await.unwrap();
        let wire::ClientFrame::Request(request) =
            wire::versioned::ClientFrame::deserialize_with_embedded_version(
                &read_frame(&mut stream).await.unwrap(),
            )
            .unwrap();
        server_received.notify_one();
        server_release.notified().await;
        let response = wire::versioned::ServerFrame::wrap_latest(wire::ServerFrame::Response(
            wire::Response {
                request_id: request.request_id,
                payload: wire::ResponsePayload::SqliteQueryOk(wire::SqliteQueryOk {
                    columns: Vec::new(),
                    rows: Vec::new(),
                    changes: 0,
                    last_insert_row_id: None,
                }),
            },
        ))
        .serialize_with_embedded_version(1)
        .unwrap();
        write_frame(&mut stream, &response).await.unwrap();
    });

    let runtime =
        agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
            .unwrap();
    let config = ActorUdsClientConfig {
        max_in_flight_requests: 1,
        ..ActorUdsClientConfig::default()
    };
    let client = ActorUdsClient::new(&path, runtime.context(), config).unwrap();
    let first_client = client.clone();
    let first = tokio::spawn(async move { first_client.query("SELECT 1", Vec::new()).await });
    received.notified().await;
    assert!(matches!(
        client.query("SELECT 2", Vec::new()).await,
        Err(ActorUdsError::ClientLimit {
            setting: "limits.sqlite.maxInFlightRequests",
            ..
        })
    ));
    release.notify_one();
    first.await.unwrap().unwrap();
    server.await.unwrap();
}
