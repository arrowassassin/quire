//! The Drop page server on picoserve: two connections on port 80, the page and its
//! JSON API from 06 §3, streaming uploads straight to the card, and the WebSocket.
//!
//! Routing is a table (`proto::routes`) driven from a single `PathRouterService`, so the
//! whole API is one match and the request body is streamed by the handler that needs it.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use embassy_futures::join::join;
use embassy_net::Stack;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{with_timeout, Duration};
use picoserve::extract::FromRequest;
use picoserve::io::{Read, Write};
use picoserve::request::{Path, Request, RequestParts};
use picoserve::response::ws::WebSocketUpgrade;
use picoserve::response::{Connection, Content, IntoResponse, Response, ResponseWriter, StatusCode};
use picoserve::routing::PathRouterService;
use picoserve::{Config, ResponseSent, Router, Server, Timeouts};
use quire_fs::{FsError, ReadAt, WriteFile};
use quire_library::{Library, Stats};
use quire_ui::{Event, Settings};

use super::{now_ms, page, ws};
use crate::proto::api::{self, StatusInfo};
use crate::proto::multipart::{self, Item, Multipart};
use crate::proto::range::{parse_content_range, plan_upload, UploadPlan};
use crate::proto::routes::{self, Route};
use crate::proto::{json, pin, settings_json, wsmsg};
use crate::{post, try_post, with, ws_broadcast, CardFs, DynFs, NetToMain, SCREEN_READY};

/// picoserve's request buffer per connection (headers; bodies stream through it).
const HTTP_BUF: usize = 4 * 1024;
/// TCP receive buffer per connection — the upload window, so twice the rest.
const TCP_RX: usize = 8 * 1024;
/// TCP send buffer per connection.
const TCP_TX: usize = 4 * 1024;
/// Uploads go to the card in blocks this size (06 §3); one block is shared by all uploads.
const BLOCK: usize = 32 * 1024;
/// Largest JSON request body (settings, rename).
const MAX_JSON_BODY: usize = 8 * 1024;
/// Largest sleep image body (a 528 × 792 P4 PBM is 52 KB).
const MAX_SLEEP_BODY: usize = 64 * 1024;

/// One 32 KB block for uploads: only one body streams to the card at a time.
static BLOCK_LOCK: Mutex<CriticalSectionRawMutex, ()> = Mutex::new(());

/// Run the two servers forever.
pub async fn serve(stack: Stack<'_>, fs: &'static dyn CardFs, hostname: &str) {
    let app = Router::from_service(DropService { fs, hostname: String::from(hostname) });
    let config = Config::new(Timeouts {
        start_read_request: Duration::from_secs(10),
        persistent_start_read_request: Duration::from_secs(5),
        read_request: Duration::from_secs(90),
        write: Duration::from_secs(15),
    })
    .keep_connection_alive();
    join(server(0, &app, &config, stack), server(1, &app, &config, stack)).await;
}

async fn server<P: picoserve::routing::PathRouter>(id: u8, app: &Router<P>, config: &Config, stack: Stack<'_>) {
    let mut http: Box<[u8]> = alloc::vec![0u8; HTTP_BUF].into_boxed_slice();
    let mut rx: Box<[u8]> = alloc::vec![0u8; TCP_RX].into_boxed_slice();
    let mut tx: Box<[u8]> = alloc::vec![0u8; TCP_TX].into_boxed_slice();
    let never = Server::new(app, config, &mut http).listen_and_serve(id, stack, 80, &mut rx, &mut tx).await;
    never.into_never()
}

/// A response the handlers build; written once at the end of the request.
enum Resp {
    Json(StatusCode, String),
    Text(StatusCode, &'static str),
    Html(String),
    Gz(&'static [u8], &'static str),
    Svg(&'static str),
    Redirect(StatusCode, String),
    File(FileContent, String),
    Empty(StatusCode),
}

impl Resp {
    fn ok_json(body: String) -> Self {
        Resp::Json(StatusCode::OK, body)
    }
    fn error(status: StatusCode, msg: &str) -> Self {
        Resp::Json(status, alloc::format!("{{\"error\":{}}}", json::quote(msg)))
    }
    fn fs_error(e: FsError) -> Self {
        match e {
            FsError::Full => Resp::error(StatusCode::INSUFFICIENT_STORAGE, "card full"),
            FsError::NotFound => Resp::error(StatusCode::NOT_FOUND, "not found"),
            other => Resp::error(StatusCode::INTERNAL_SERVER_ERROR, &alloc::format!("{other:?}")),
        }
    }
}

impl IntoResponse for Resp {
    async fn write_to<R: Read, W: ResponseWriter<Error = R::Error>>(
        self,
        conn: Connection<'_, R>,
        rw: W,
    ) -> Result<ResponseSent, W::Error> {
        match self {
            Resp::Json(st, body) => {
                rw.write_response(
                    conn,
                    Response::new(st, body).with_content_type("application/json").with_header("Cache-Control", "no-store"),
                )
                .await
            }
            Resp::Text(st, t) => rw.write_response(conn, Response::new(st, t)).await,
            Resp::Html(h) => {
                rw.write_response(
                    conn,
                    Response::ok(h).with_content_type("text/html; charset=utf-8").with_header("Cache-Control", "no-store"),
                )
                .await
            }
            Resp::Gz(bytes, ct) => {
                rw.write_response(
                    conn,
                    Response::ok(bytes).with_content_type(ct).with_headers([("Content-Encoding", "gzip"), ("Cache-Control", "no-cache")]),
                )
                .await
            }
            Resp::Svg(s) => {
                rw.write_response(conn, Response::ok(s).with_content_type("image/svg+xml").with_header("Cache-Control", "max-age=86400"))
                    .await
            }
            Resp::Redirect(st, loc) => {
                rw.write_response(conn, Response::empty(st).with_headers([("Location", loc), ("Cache-Control", String::from("no-store"))]))
                    .await
            }
            Resp::File(f, name) => {
                rw.write_response(
                    conn,
                    Response::ok(f).with_header("Content-Disposition", alloc::format!("attachment; filename=\"{name}\"")),
                )
                .await
            }
            Resp::Empty(st) => rw.write_response(conn, Response::empty(st)).await,
        }
    }
}

/// A card file streamed in 4 KB pieces.
struct FileContent {
    file: Box<dyn ReadAt>,
    len: usize,
    content_type: &'static str,
}

impl Content for FileContent {
    fn content_type(&self) -> &'static str {
        self.content_type
    }
    fn content_length(&self) -> usize {
        self.len
    }
    async fn write_content<W: Write>(self, mut writer: W) -> Result<(), W::Error> {
        let mut buf = alloc::vec![0u8; 4096];
        let mut off = 0usize;
        while off < self.len {
            let want = buf.len().min(self.len - off);
            let n = match self.file.read_at(off as u64, &mut buf[..want]) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            writer.write_all(&buf[..n]).await?;
            off += n;
        }
        writer.flush().await
    }
}

fn content_type_for(path: &str) -> &'static str {
    match quire_fs::extension(path).as_str() {
        "epub" | "kepub" => "application/epub+zip",
        "txt" | "log" => "text/plain; charset=utf-8",
        "md" | "markdown" => "text/markdown; charset=utf-8",
        "html" | "htm" | "xhtml" => "text/html; charset=utf-8",
        "fb2" | "xml" | "opf" => "application/xml",
        "cbz" | "zip" => "application/zip",
        "pbm" => "image/x-portable-bitmap",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

/// Read a whole small body (JSON), refusing large ones.
async fn read_small<R: Read>(reader: &mut R, len: usize, max: usize) -> Result<Vec<u8>, ()> {
    if len > max {
        return Err(());
    }
    let mut out = alloc::vec![0u8; len];
    let mut got = 0;
    while got < len {
        match reader.read(&mut out[got..]).await {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(_) => return Err(()),
        }
    }
    out.truncate(got);
    Ok(out)
}

/// Why a streamed upload stopped.
enum StreamError {
    /// The client went away.
    Io,
    /// The card refused a write.
    Fs(FsError),
}

/// Stream `len` body bytes through the shared 32 KB block into `sink`; `progress` gets
/// the running total about once a second.
async fn stream_body<R: Read>(
    reader: &mut R,
    len: usize,
    sink: &mut dyn FnMut(&[u8]) -> Result<(), FsError>,
    progress: &mut dyn FnMut(usize),
) -> Result<usize, StreamError> {
    let _guard = BLOCK_LOCK.lock().await;
    let mut block = alloc::vec![0u8; BLOCK.min(len.max(512))];
    let mut received = 0usize;
    let mut last_report = now_ms();
    while received < len {
        let mut filled = 0usize;
        while filled < block.len() && received + filled < len {
            let want = (block.len() - filled).min(len - received - filled);
            match reader.read(&mut block[filled..filled + want]).await {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(_) => return Err(StreamError::Io),
            }
        }
        if filled == 0 {
            return Err(StreamError::Io);
        }
        sink(&block[..filled]).map_err(StreamError::Fs)?;
        received += filled;
        let now = now_ms();
        if now.wrapping_sub(last_report) >= 1000 {
            last_report = now;
            progress(received);
        }
        crate::touch(now);
    }
    Ok(received)
}

/// The service behind the router.
struct DropService {
    fs: &'static dyn CardFs,
    hostname: String,
}

impl DropService {
    fn dfs(&self) -> DynFs<'static> {
        DynFs(self.fs)
    }

    fn pin_ok(&self, parts: &RequestParts<'_>) -> bool {
        let configured = crate::load_config(self.fs).pin;
        if configured.is_empty() {
            return true;
        }
        let header = parts.headers().get("x-pin").and_then(|h| h.as_str().ok());
        let query = routes::query_param(parts.query().map(|q| q.0), "pin");
        pin::check(&configured, header, query.as_deref())
    }

    fn status(&self) -> Resp {
        let (info, wifi, total, pin_set) = with(|i| (i.status.clone(), i.wifi.clone(), i.card_total, i.pin_set));
        Resp::ok_json(api::status_json(&info, &wifi, self.fs.free_bytes(), total, pin_set))
    }

    fn library(&self) -> Resp {
        Resp::ok_json(api::library_json(&Library::load(&self.dfs())))
    }

    fn stats(&self) -> Resp {
        let dfs = self.dfs();
        let stats = Stats::load(&dfs);
        let now = with(|i| i.status.local_now);
        let today = quire_library::time::day_of(now);
        let lib = Library::load(&dfs);
        let sessions = api::with_titles(&lib, stats.recent(&dfs, 40));
        Resp::ok_json(api::stats_json(&stats, today, &sessions))
    }

    fn settings_get(&self) -> Resp {
        let pin_set = with(|i| i.pin_set);
        Resp::ok_json(settings_json::to_json(&Settings::load(&self.dfs()), pin_set))
    }

    /// `POST /api/fetch` with `{"url": "...", "title": "...", "author": "..."}`: queue a
    /// download of a book from the phone's clipboard; it runs while the station session
    /// is up and shows on the Downloads screen like any Bookshop download.
    async fn fetch(&self, body: &[u8]) -> Resp {
        let Ok(text) = core::str::from_utf8(body) else { return Resp::error(StatusCode::BAD_REQUEST, "not UTF-8") };
        let Some(v) = crate::proto::jsonlite::parse(text.as_bytes()) else { return Resp::error(StatusCode::BAD_REQUEST, "not JSON") };
        let url = v.get("url").and_then(|u| u.as_str()).unwrap_or_default();
        if !(url.starts_with("http://") || url.starts_with("https://")) || url.len() > 1024 {
            return Resp::error(StatusCode::BAD_REQUEST, "url must be http or https");
        }
        let title = v.get("title").and_then(|t| t.as_str()).filter(|t| !t.is_empty()).unwrap_or_else(|| {
            url.rsplit('/')
                .next()
                .map(|s| s.split('?').next().unwrap_or(s))
                .filter(|s| !s.is_empty())
                .map(String::from)
                .unwrap_or_else(|| String::from("Download"))
        });
        let author = v.get("author").and_then(|a| a.as_str()).unwrap_or_default();
        let online = matches!(crate::wifi_state(), quire_ui::WifiState::Connected { .. });
        super::fetch::enqueue(crate::NetCommand::Fetch(quire_ui::net::FetchRequest::Book { url, title, author, size: None }), online).await;
        Resp::Text(StatusCode::OK, "queued")
    }

    async fn settings_put(&self, body: &[u8]) -> Resp {
        let Ok(text) = core::str::from_utf8(body) else { return Resp::error(StatusCode::BAD_REQUEST, "not UTF-8") };
        let dfs = self.dfs();
        let current = Settings::load(&dfs);
        let merged = match settings_json::merge(&current, text) {
            Ok(m) => m,
            Err(e) => return Resp::error(StatusCode::BAD_REQUEST, &e),
        };
        if let Err(e) = merged.settings.save(&dfs) {
            return Resp::fs_error(e);
        }
        if let Some(p) = merged.pin {
            let mut cfg = crate::load_config(self.fs);
            cfg.pin = p;
            crate::save_config(self.fs, &cfg);
        }
        post(NetToMain::SettingsChanged).await;
        let pin_set = with(|i| i.pin_set);
        Resp::ok_json(settings_json::to_json(&merged.settings, pin_set))
    }

    fn file_get(&self, path: &str) -> Resp {
        match self.fs.open(path) {
            Ok(file) => {
                let len = file.len() as usize;
                let name = String::from(quire_fs::file_name(path));
                Resp::File(FileContent { file, len, content_type: content_type_for(path) }, name)
            }
            Err(e) => Resp::fs_error(e),
        }
    }

    async fn file_delete(&self, path: &str) -> Resp {
        let part = alloc::format!("{path}.part");
        if self.fs.exists(&part) {
            let _ = self.fs.remove(&part);
        }
        match self.fs.remove(path) {
            Ok(()) => {
                post(NetToMain::Ui(Event::BooksChanged)).await;
                Resp::Empty(StatusCode::NO_CONTENT)
            }
            Err(e) => Resp::fs_error(e),
        }
    }

    async fn file_rename(&self, path: &str, body: &[u8]) -> Resp {
        let to = core::str::from_utf8(body)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
            .and_then(|v| v.get("to").and_then(|t| t.as_str()).map(String::from));
        let Some(to) = to else { return Resp::error(StatusCode::BAD_REQUEST, "expected {\"to\": name}") };
        let parent = quire_fs::parent(path);
        let Some(new_path) = routes::sanitize_path(&alloc::format!("{parent}/{to}")) else {
            return Resp::error(StatusCode::BAD_REQUEST, "bad name");
        };
        if to.contains('/') || new_path == path {
            return Resp::error(StatusCode::BAD_REQUEST, "bad name");
        }
        match self.fs.rename(path, &new_path) {
            Ok(()) => {
                post(NetToMain::Ui(Event::BooksChanged)).await;
                let mut j = json::Json::new();
                j.obj().kv_str("path", &new_path).end();
                Resp::ok_json(j.finish())
            }
            Err(e) => Resp::fs_error(e),
        }
    }

    /// `PUT /api/files/<path>`: raw body, optional `Content-Range`, streamed to
    /// `<path>.part` and renamed into place when complete.
    async fn file_put<R: Read>(&self, path: &str, parts: &RequestParts<'_>, reader: &mut R, content_length: usize) -> Resp {
        let range = parts.headers().get("content-range").and_then(|h| h.as_str().ok()).and_then(parse_content_range);
        let part = alloc::format!("{path}.part");
        let have = self.fs.open(&part).ok().map(|f| f.len());
        let plan = match plan_upload(content_length as u64, range, have) {
            Ok(p) => p,
            Err(e) => return Resp::error(StatusCode::BAD_REQUEST, e),
        };
        let name = String::from(quire_fs::file_name(path));
        let (writer, last, start) = match plan {
            UploadPlan::Mismatch { have } => {
                let mut j = json::Json::new();
                j.obj().kv_u64("received", have).kv_bool("done", false).end();
                return Resp::Json(StatusCode::CONFLICT, j.finish());
            }
            UploadPlan::Fresh { last, .. } => {
                let parent = quire_fs::parent(path);
                if !parent.is_empty() && !self.fs.exists(parent) {
                    if let Err(e) = self.fs.mkdir_all(parent) {
                        return Resp::fs_error(e);
                    }
                }
                match self.fs.create(&part) {
                    Ok(w) => (w, last, 0u64),
                    Err(e) => return Resp::fs_error(e),
                }
            }
            UploadPlan::Append { last, .. } => match self.fs.append(&part) {
                Ok(w) => (w, last, have.unwrap_or(0)),
                Err(e) => return Resp::fs_error(e),
            },
        };
        let total = range.and_then(|r| r.total).unwrap_or(content_length as u64);
        crate::transfer_started(&name, path, Some(total), start);
        try_post(crate::downloads_event_msg());
        let result = {
            let mut writer = writer;
            let mut sink = |chunk: &[u8]| writer.write_all(chunk);
            let name2 = name.clone();
            let mut progress = |got: usize| {
                let done = start + got as u64;
                crate::transfer_progress(&name2, done);
                try_post(crate::downloads_event_msg());
                ws_broadcast(wsmsg::upload_event(&name2, done, total, "uploading"));
            };
            let r = stream_body(reader, content_length, &mut sink, &mut progress).await;
            if r.is_ok() {
                if let Err(e) = writer.flush() {
                    crate::transfer_finished(&name, Err(alloc::format!("{e:?}")));
                    return Resp::fs_error(e);
                }
            }
            r
        };
        match result {
            Err(StreamError::Io) => {
                crate::transfer_finished(&name, Err(String::from("connection lost")));
                try_post(crate::downloads_event_msg());
                Resp::error(StatusCode::BAD_REQUEST, "body ended early")
            }
            Err(StreamError::Fs(e)) => {
                crate::transfer_finished(&name, Err(alloc::format!("{e:?}")));
                try_post(crate::downloads_event_msg());
                Resp::fs_error(e)
            }
            Ok(got) => {
                let received = start + got as u64;
                if last {
                    if self.fs.exists(path) {
                        let _ = self.fs.remove(path);
                    }
                    if let Err(e) = self.fs.rename(&part, path) {
                        crate::transfer_finished(&name, Err(alloc::format!("{e:?}")));
                        return Resp::fs_error(e);
                    }
                    crate::transfer_finished(&name, Ok(()));
                    post(crate::downloads_event_msg()).await;
                    post(NetToMain::Ui(Event::BooksChanged)).await;
                    ws_broadcast(wsmsg::upload_event(&name, received, total, "done"));
                    ws_broadcast(wsmsg::books_event(1));
                } else {
                    crate::transfer_progress(&name, received);
                    try_post(crate::downloads_event_msg());
                }
                let mut j = json::Json::new();
                j.obj().kv_u64("received", received).kv_bool("done", last).end();
                Resp::Json(if last { StatusCode::CREATED } else { StatusCode::OK }, j.finish())
            }
        }
    }

    /// `POST /upload`: multipart fallback (iOS Shortcut, share target, old browsers).
    async fn upload_multipart<R: Read>(&self, parts: &RequestParts<'_>, reader: &mut R, content_length: usize) -> Resp {
        let boundary = parts.headers().get("content-type").and_then(|h| h.as_str().ok()).and_then(multipart::boundary_from_content_type);
        let Some(boundary) = boundary else { return Resp::error(StatusCode::BAD_REQUEST, "expected multipart/form-data") };
        let wants_html = parts.headers().get("accept").and_then(|h| h.as_str().ok()).is_some_and(|a| a.contains("text/html"));
        let fs = self.fs;
        let mut mp = Multipart::new(&boundary);
        // The splitter's sink writes synchronously; state and the first error live here.
        struct PartState {
            writer: Option<Box<dyn WriteFile>>,
            path: String,
            added: Vec<String>,
            error: Option<FsError>,
        }
        let mut st = PartState { writer: None, path: String::new(), added: Vec::new(), error: None };
        {
            let mut sink = |chunk: &[u8]| -> Result<(), FsError> {
                let mut on_item = |item: Item<'_>| {
                    if st.error.is_some() {
                        return;
                    }
                    match item {
                        Item::Headers { filename, .. } => {
                            st.writer = None;
                            st.path.clear();
                            let Some(f) = filename else { return };
                            let Some(p) = routes::sanitize_path(&f) else { return };
                            let parent = quire_fs::parent(&p);
                            if !parent.is_empty() && !fs.exists(parent) {
                                let _ = fs.mkdir_all(parent);
                            }
                            match fs.create(&alloc::format!("{p}.part")) {
                                Ok(w) => {
                                    st.writer = Some(w);
                                    st.path = p.clone();
                                    crate::transfer_started(quire_fs::file_name(&p), &p, None, 0);
                                }
                                Err(e) => st.error = Some(e),
                            }
                        }
                        Item::Data(d) => {
                            if let Some(w) = st.writer.as_mut() {
                                if let Err(e) = w.write_all(d) {
                                    st.error = Some(e);
                                }
                            }
                        }
                        Item::End => {
                            if let Some(mut w) = st.writer.take() {
                                let path = core::mem::take(&mut st.path);
                                let part = alloc::format!("{path}.part");
                                let r = w.flush();
                                drop(w);
                                let r = r.and_then(|_| {
                                    if fs.exists(&path) {
                                        let _ = fs.remove(&path);
                                    }
                                    fs.rename(&part, &path)
                                });
                                match r {
                                    Ok(()) => {
                                        crate::transfer_finished(quire_fs::file_name(&path), Ok(()));
                                        st.added.push(path);
                                    }
                                    Err(e) => {
                                        crate::transfer_finished(quire_fs::file_name(&path), Err(alloc::format!("{e:?}")));
                                        st.error = Some(e);
                                    }
                                }
                            }
                        }
                    }
                };
                mp.feed(chunk, &mut on_item);
                match st.error.take() {
                    Some(e) => {
                        st.error = Some(e.clone());
                        Err(e)
                    }
                    None => Ok(()),
                }
            };
            let mut progress = |_got: usize| {
                try_post(crate::downloads_event_msg());
            };
            match stream_body(reader, content_length, &mut sink, &mut progress).await {
                Ok(_) => {}
                Err(StreamError::Io) => return Resp::error(StatusCode::BAD_REQUEST, "body ended early"),
                Err(StreamError::Fs(e)) => return Resp::fs_error(e),
            }
        }
        let added = st.added;
        if !added.is_empty() {
            post(crate::downloads_event_msg()).await;
            post(NetToMain::Ui(Event::BooksChanged)).await;
            ws_broadcast(wsmsg::books_event(added.len() as u32));
        }
        if wants_html {
            return Resp::Redirect(StatusCode::SEE_OTHER, alloc::format!("/?added={}", added.len()));
        }
        let mut j = json::Json::new();
        j.obj().key("added").arr();
        for p in &added {
            j.str(p);
        }
        j.end().end();
        Resp::ok_json(j.finish())
    }

    /// `POST /sleep?name=`: a dithered 1-bit PBM into the sleep folder.
    async fn sleep_put<R: Read>(&self, parts: &RequestParts<'_>, reader: &mut R, content_length: usize) -> Resp {
        let Some(name) = routes::query_param(parts.query().map(|q| q.0), "name") else {
            return Resp::error(StatusCode::BAD_REQUEST, "name=");
        };
        if content_length > MAX_SLEEP_BODY {
            return Resp::error(StatusCode::PAYLOAD_TOO_LARGE, "too large for a sleep image");
        }
        let folder = Settings::load(&self.dfs()).sleep_folder;
        let mut name = name.replace('/', "_");
        if !name.ends_with(".pbm") {
            name.push_str(".pbm");
        }
        let Some(path) = routes::sanitize_path(&alloc::format!("{folder}/{name}")) else {
            return Resp::error(StatusCode::BAD_REQUEST, "bad name");
        };
        if !self.fs.exists(&folder) {
            let _ = self.fs.mkdir_all(&folder);
        }
        let mut writer = match self.fs.create(&path) {
            Ok(w) => w,
            Err(e) => return Resp::fs_error(e),
        };
        let mut sink = |c: &[u8]| writer.write_all(c);
        let mut progress = |_: usize| {};
        match stream_body(reader, content_length, &mut sink, &mut progress).await {
            Ok(_) => match writer.flush() {
                Ok(()) => {
                    let mut j = json::Json::new();
                    j.obj().kv_str("path", &path).end();
                    Resp::Json(StatusCode::CREATED, j.finish())
                }
                Err(e) => Resp::fs_error(e),
            },
            Err(StreamError::Io) => Resp::error(StatusCode::BAD_REQUEST, "body ended early"),
            Err(StreamError::Fs(e)) => Resp::fs_error(e),
        }
    }

    /// `GET /api/screen.pbm`: ask the main loop for the frame, then stream the file.
    async fn screen(&self) -> Resp {
        SCREEN_READY.reset();
        if !try_post(NetToMain::ScreenRequest) {
            return Resp::error(StatusCode::SERVICE_UNAVAILABLE, "busy");
        }
        match with_timeout(Duration::from_secs(4), SCREEN_READY.wait()).await {
            Ok(true) => self.file_get(crate::SCREEN_FILE),
            _ => Resp::error(StatusCode::SERVICE_UNAVAILABLE, "no frame"),
        }
    }
}

impl PathRouterService for DropService {
    async fn call_path_router_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        _state: &(),
        _params: (),
        path: Path<'_>,
        mut request: Request<'_, R>,
        rw: W,
    ) -> Result<ResponseSent, W::Error> {
        crate::touch(now_ms());
        let parts = request.parts;
        let route = routes::route(parts.method(), path.encoded());
        log::debug!("http: {} {}", parts.method(), path.encoded());

        if let Route::Ws = route {
            let upgrade = WebSocketUpgrade::from_request(&(), parts, request.body_connection.body()).await;
            let conn = request.body_connection.finalize().await?;
            return match upgrade {
                Ok(u) => u.on_upgrade(ws::WsClient).write_to(conn, rw).await,
                Err(rej) => rej.write_to(conn, rw).await,
            };
        }
        if route.is_write() && !self.pin_ok(&parts) {
            let conn = request.body_connection.finalize().await?;
            return Resp::error(StatusCode::FORBIDDEN, "PIN required").write_to(conn, rw).await;
        }

        let content_length = request.body_connection.content_length();
        let resp = match route {
            Route::Index => Resp::Gz(page::INDEX_GZ, "text/html; charset=utf-8"),
            Route::Manifest => Resp::Gz(page::MANIFEST_GZ, "application/manifest+json"),
            Route::ServiceWorker => Resp::Gz(page::SW_GZ, "text/javascript"),
            Route::Icon => Resp::Svg(api::ICON_SVG),
            Route::Captive => {
                let ip = with(|i| match &i.wifi {
                    quire_ui::WifiState::Hotspot { ip, .. } | quire_ui::WifiState::Connected { ip, .. } => ip.clone(),
                    _ => super::wifi::ap_ip_text(),
                });
                Resp::Html(api::captive_html(&ip, &self.hostname))
            }
            Route::CaptiveProbe => Resp::Redirect(StatusCode::FOUND, alloc::format!("http://{}/captive", super::wifi::ap_ip_text())),
            // iOS and macOS are told what they are hoping to hear, so that they
            // stay on a network whose whole purpose is the reader at the other
            // end of it. It is not true — there is no route past this device —
            // and the cost is that neither will offer its sign-in sheet, so the
            // Drop page has to be opened by hand or by scanning the code.
            Route::AppleProbe => Resp::Html(String::from(routes::APPLE_SUCCESS)),
            Route::Status => self.status(),
            Route::Library => self.library(),
            Route::Stats => self.stats(),
            Route::SettingsGet => self.settings_get(),
            Route::SettingsPut => {
                let body = read_small(&mut request.body_connection.body().reader(), content_length, MAX_JSON_BODY).await;
                match body {
                    Ok(b) => self.settings_put(&b).await,
                    Err(()) => Resp::error(StatusCode::PAYLOAD_TOO_LARGE, "settings body too large"),
                }
            }
            Route::FileGet(p) => self.file_get(&p),
            Route::FileDelete(p) => self.file_delete(&p).await,
            Route::FileRename(p) => {
                let body = read_small(&mut request.body_connection.body().reader(), content_length, MAX_JSON_BODY).await;
                match body {
                    Ok(b) => self.file_rename(&p, &b).await,
                    Err(()) => Resp::error(StatusCode::PAYLOAD_TOO_LARGE, "body too large"),
                }
            }
            Route::FilePut(p) => {
                let mut reader = request.body_connection.body().reader();
                self.file_put(&p, &parts, &mut reader, content_length).await
            }
            Route::Upload => {
                let mut reader = request.body_connection.body().reader();
                self.upload_multipart(&parts, &mut reader, content_length).await
            }
            Route::Sleep => {
                let mut reader = request.body_connection.body().reader();
                self.sleep_put(&parts, &mut reader, content_length).await
            }
            Route::Screen => self.screen().await,
            Route::Fetch => {
                let body = read_small(&mut request.body_connection.body().reader(), content_length, MAX_JSON_BODY).await;
                match body {
                    Ok(b) => self.fetch(&b).await,
                    Err(()) => Resp::error(StatusCode::PAYLOAD_TOO_LARGE, "body too large"),
                }
            }
            Route::Ws => unreachable!(),
            Route::NotFound => Resp::Text(StatusCode::NOT_FOUND, "Not found"),
            Route::MethodNotAllowed => Resp::Text(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed"),
        };
        let conn = request.body_connection.finalize().await?;
        resp.write_to(conn, rw).await
    }
}

/// Unused type alias kept for the handlers' signatures.
#[allow(dead_code)]
type Info = StatusInfo;
