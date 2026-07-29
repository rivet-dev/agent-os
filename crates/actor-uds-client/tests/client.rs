use std::io;

use agentos_actor_uds_client::protocol as wire;
use agentos_actor_uds_client::{ActorUdsClient, ActorUdsError, SqlValue};
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
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

    let client = ActorUdsClient::new(&path);
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

    let error = ActorUdsClient::new(&path)
        .query("SELECT 1", Vec::new())
        .await
        .unwrap_err();
    assert!(matches!(error, ActorUdsError::VersionMismatch));
    server.await.unwrap();
}
