use crate::context_replica::*;
use crate::project_context::*;
use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
async fn server(store: Store) -> (String, tokio::task::JoinHandle<()>) {
    let routes =
        Router::new()
            .route(
                "/api/projects/test/context/manifest",
                get(|State(s): State<Store>| async move { Json(s.manifest().unwrap()) }),
            )
            .route(
                "/api/projects/test/context/conflicts",
                get(|State(s): State<Store>| async move { Json(s.conflicts().unwrap()) }),
            )
            .route(
                "/api/projects/test/context/blobs",
                post(
                    |State(s): State<Store>, body: axum::body::Bytes| async move {
                        Json(s.put_blob(&body).unwrap())
                    },
                ),
            )
            .route(
                "/api/projects/test/context/blobs/:hash",
                get(
                    |State(s): State<Store>, Path(hash): Path<String>| async move {
                        s.blob(&hash).unwrap()
                    },
                ),
            )
            .route(
                "/api/projects/test/context/operations",
                post(
                    |State(s): State<Store>, Json(op): Json<Operation>| async move {
                        Json(s.apply(op).unwrap())
                    },
                ),
            )
            .with_state(store);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    (
        format!("http://{address}"),
        tokio::spawn(async move {
            axum::serve(listener, routes).await.unwrap();
        }),
    )
}
fn replica(base: &std::path::Path, name: &str, endpoint: &str) -> Replica {
    Replica {
        store: Store::new(base.join(name).join("files"), base.join(name).join("state")),
        endpoint: endpoint.into(),
        token: "test".into(),
        project: "test".into(),
        source: name.into(),
    }
}
#[tokio::test]
async fn round_trip_and_concurrent_edits() {
    let dir = tempfile::tempdir().unwrap();
    let core = Store::new(dir.path().join("core"), dir.path().join("state"));
    core.manifest().unwrap();
    std::fs::write(core.root.join("note.md"), "original").unwrap();
    let (endpoint, server) = server(core.clone()).await;
    let a = replica(dir.path(), "a", &endpoint);
    let b = replica(dir.path(), "b", &endpoint);
    assert!(a.tick().await.unwrap().initialized);
    assert!(b.tick().await.unwrap().initialized);
    assert_eq!(
        std::fs::read_to_string(a.store.root.join("note.md")).unwrap(),
        "original"
    );
    std::fs::write(a.store.root.join("note.md"), "first").unwrap();
    std::fs::write(b.store.root.join("note.md"), "second").unwrap();
    assert!(a.tick().await.unwrap().error.is_none());
    assert_eq!(b.tick().await.unwrap().conflicts.len(), 1);
    assert_eq!(
        std::fs::read_to_string(core.root.join("note.md")).unwrap(),
        "first"
    );
    assert_eq!(
        std::fs::read_to_string(b.store.root.join("note.md")).unwrap(),
        "second"
    );
    let history = core.history().unwrap();
    assert!(history.len() >= 2);
    server.abort();
    std::fs::write(a.store.root.join("offline.md"), "queued").unwrap();
    let status = a.tick().await.unwrap();
    assert!(status.error.is_some());
    assert_eq!(status.pending.len(), 1);
    let mut a = replica(dir.path(), "a", &endpoint);
    assert_eq!(a.status().unwrap().pending.len(), 1);
    let (endpoint, server) = self::server(core.clone()).await;
    a.endpoint = endpoint;
    assert!(a.tick().await.unwrap().pending.is_empty());
    assert_eq!(
        std::fs::read_to_string(core.root.join("offline.md")).unwrap(),
        "queued"
    );
    server.abort();
}

#[tokio::test]
async fn context_file_folder_transitions_and_conflict_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let core = Store::new(dir.path().join("core"), dir.path().join("state"));
    core.manifest().unwrap();
    std::fs::write(core.root.join("item"), "file").unwrap();
    let (endpoint, server) = server(core.clone()).await;
    let a = replica(dir.path(), "a", &endpoint);
    assert!(a.tick().await.unwrap().ready);
    std::fs::remove_file(a.store.root.join("item")).unwrap();
    std::fs::create_dir(a.store.root.join("item")).unwrap();
    std::fs::write(a.store.root.join("item/note.md"), "inside").unwrap();
    for _ in 0..3 {
        assert!(a.tick().await.unwrap().error.is_none());
    }
    assert_eq!(
        std::fs::read_to_string(core.root.join("item/note.md")).unwrap(),
        "inside"
    );
    std::fs::remove_dir_all(core.root.join("item")).unwrap();
    std::fs::write(core.root.join("item"), "file again").unwrap();
    assert!(a.tick().await.unwrap().error.is_none());
    assert_eq!(
        std::fs::read_to_string(a.store.root.join("item")).unwrap(),
        "file again"
    );
    let b = replica(dir.path(), "b", &endpoint);
    assert!(b.tick().await.unwrap().ready);
    std::fs::write(core.root.join("item"), "shared").unwrap();
    std::fs::write(b.store.root.join("item"), "variant").unwrap();
    assert_eq!(b.tick().await.unwrap().conflicts.len(), 1);
    let conflicts = core.conflicts().unwrap();
    let (id, op) = conflicts.iter().next().unwrap();
    let current = core.manifest().unwrap();
    core.resolve(
        id,
        Operation {
            id: "resolution".into(),
            base: Some(current.entries["item"].revision),
            ..op.clone()
        },
    )
    .unwrap();
    assert!(b.tick().await.unwrap().conflicts.is_empty());
    assert_eq!(
        std::fs::read_to_string(core.root.join("item")).unwrap(),
        "variant"
    );
    server.abort();
}
