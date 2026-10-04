//! Client-side jobs: the worker runs them one at a time while a station session is up
//! (downloads into the library, the Bookshop catalog, OPDS feeds, Wikipedia, weather,
//! news, sleep packs, the update check and download); `Cancel`/`Retry` act on the shared
//! list at once. Every job is boxed for its run so the net task's own future stays small.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;

use embassy_net::Stack;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use quire_fs::{Fs, WriteFile};
use quire_ui::net::{DownloadState, FetchRequest, NetEvent, OtaInfo};
use quire_ui::screens::apps::news::{self, ArticleMeta};
use quire_ui::screens::bookshop::{CatalogFile, CATALOG_FILE};
use quire_ui::{Event, Settings};

use super::client::{self, Error, Head, Request};
use super::{calibre, now_ms, sync};
use crate::proto::{feeds, github, opds, sleepidx, weather, wiki};
use crate::{post, try_post, with, CardFs, DynFs, NetCommand, NetToMain};

/// The Bookshop catalog, in the card's own format.
const CATALOG_URL: &str = "https://raw.githubusercontent.com/arrowassassin/quire/main/bookshop/catalog.bin";
/// Where an update image lands (the board's `install_from_card` reads it).
pub const UPDATE_FILE: &str = "/quire/update.bin";
/// Longest OPDS 2.0 JSON feed kept.
const OPDS_JSON_MAX: usize = 48 * 1024;
/// Longest Wikipedia summary document kept.
const WIKI_MAX: usize = 12 * 1024;
/// Longest weather document kept.
const WEATHER_MAX: usize = 6 * 1024;
/// Tries for a rate-limited download (429/503), 30 s apart.
const DOWNLOAD_TRIES: u8 = 5;
const RETRY_WAIT_S: u16 = 30;

/// A job for the worker.
enum Job {
    /// Run the download queue.
    Downloads,
    Shelves,
    Opds(String),
    Wikipedia(String),
    Weather,
    News,
    SleepPacks,
    SleepPack(String),
    OtaCheck,
    Ota(String),
    Sync,
}

static JOBS: Channel<CriticalSectionRawMutex, Job, 6> = Channel::new();

/// The last update check, for the checksum file of the download that follows.
static LAST_RELEASE: critical_section::Mutex<core::cell::RefCell<Option<github::Release>>> =
    critical_section::Mutex::new(core::cell::RefCell::new(None));

const OFFLINE: &str = "Wi-Fi is off. Turn it on from the Power menu.";

async fn ui(ev: NetEvent) {
    post(NetToMain::Ui(Event::Net(ev))).await;
}

/// Take a command from the main loop. `online` says a station session is running (a
/// worker exists); otherwise the answer is immediate.
pub async fn enqueue(cmd: NetCommand, online: bool) {
    let job = match cmd {
        NetCommand::Fetch(FetchRequest::Cancel(url)) => {
            crate::cancel_download(&url);
            try_post(crate::downloads_event_msg());
            return;
        }
        NetCommand::Fetch(FetchRequest::Retry(url)) => {
            if crate::retry_download(&url) {
                try_post(crate::downloads_event_msg());
                if online {
                    let _ = JOBS.try_send(Job::Downloads);
                }
            }
            return;
        }
        NetCommand::Fetch(FetchRequest::Book { url, title, author, size }) => {
            crate::download_queued(&title, &author, &url, size);
            if !online {
                crate::download_state(&title, DownloadState::Failed(String::from("Wi-Fi is off")));
            }
            try_post(crate::downloads_event_msg());
            Job::Downloads
        }
        NetCommand::Fetch(FetchRequest::Shelves) => Job::Shelves,
        NetCommand::Fetch(FetchRequest::Opds(u)) => Job::Opds(u),
        NetCommand::Fetch(FetchRequest::Wikipedia(t)) => Job::Wikipedia(t),
        NetCommand::Fetch(FetchRequest::Weather) => Job::Weather,
        NetCommand::Fetch(FetchRequest::News) => Job::News,
        NetCommand::Fetch(FetchRequest::SleepPacks) => Job::SleepPacks,
        NetCommand::Fetch(FetchRequest::SleepPack(id)) => Job::SleepPack(id),
        NetCommand::Fetch(FetchRequest::OtaCheck) => Job::OtaCheck,
        NetCommand::Ota(src) => Job::Ota(src),
        NetCommand::SyncNow => Job::Sync,
        NetCommand::Calibre(on) => {
            calibre::set(on && online);
            if !online {
                with(|i| i.calibre_status = String::from("Calibre server: Wi-Fi is off"));
                ui(NetEvent::Calibre(String::from(OFFLINE))).await;
            }
            return;
        }
        _ => return,
    };
    if !online {
        offline_reply(job).await;
        return;
    }
    if JOBS.try_send(job).is_err() {
        log::warn!("fetch: queue full");
    }
}

/// The answer a job gets when there is no session.
async fn offline_reply(job: Job) {
    let err = String::from(OFFLINE);
    match job {
        Job::Downloads => ui(NetEvent::Downloads).await,
        Job::Shelves => ui(NetEvent::Shelves).await,
        Job::Opds(_) => ui(NetEvent::Opds(Err(err))).await,
        Job::Wikipedia(_) => ui(NetEvent::Wikipedia(Err(err))).await,
        Job::Weather => ui(NetEvent::Weather).await,
        Job::News => ui(NetEvent::News).await,
        Job::SleepPacks | Job::SleepPack(_) => ui(NetEvent::SleepPacks).await,
        Job::OtaCheck => ui(NetEvent::Ota(Err(err))).await,
        Job::Ota(_) => ui(NetEvent::OtaProgress { done: 0, total: 0, finished: Some(Err(err)) }).await,
        Job::Sync => ui(NetEvent::Sync(Err(err))).await,
    }
}

/// The worker: runs while the station session does.
pub async fn worker(stack: Stack<'_>, fs: &'static dyn CardFs) {
    loop {
        let job = JOBS.receive().await;
        let tz = Settings::load(&DynFs(fs)).tz_minutes;
        with(|i| i.tz_minutes = tz);
        let before = esp_alloc::HEAP.free();
        // Boxed: the job's state (TLS buffers aside) lives on the heap for its run only.
        let fut: Pin<Box<dyn Future<Output = ()> + '_>> = match job {
            Job::Downloads => Box::pin(run_downloads(stack, fs)),
            Job::Shelves => Box::pin(shelves(stack, fs)),
            Job::Opds(u) => Box::pin(opds_feed(stack, u)),
            Job::Wikipedia(t) => Box::pin(wikipedia(stack, fs, t)),
            Job::Weather => Box::pin(weather_job(stack, fs)),
            Job::News => Box::pin(news_job(stack, fs)),
            Job::SleepPacks => Box::pin(sleep_packs(stack, fs)),
            Job::SleepPack(id) => Box::pin(sleep_pack(stack, fs, id)),
            Job::OtaCheck => Box::pin(ota_check(stack)),
            Job::Ota(src) => Box::pin(ota_download(stack, fs, src)),
            Job::Sync => Box::pin(sync::sync_now(stack, fs)),
        };
        fut.await;
        log::info!("fetch: job done, heap free {} (was {before})", esp_alloc::HEAP.free());
    }
}

// ---------------------------------------------------------------------------------------
// Files on the card

/// Stream a URL to `path` through `path.part`, resuming a partial file, with progress
/// through `progress(done, total)`. `cancel` is polled per piece.
async fn download_file(
    stack: Stack<'_>,
    url: &str,
    path: &str,
    headers: &[(&str, &str)],
    fs: &'static dyn CardFs,
    progress: &mut dyn FnMut(u64, Option<u64>),
    cancel: &mut dyn FnMut() -> bool,
) -> Result<Head, Error> {
    let part = alloc::format!("{path}.part");
    let parent = quire_fs::parent(path);
    if !parent.is_empty() && !fs.exists(parent) {
        fs.mkdir_all(parent).map_err(|e| Error::Sink(alloc::format!("card: {e:?}")))?;
    }
    let have = fs.open(&part).ok().map(|f| f.len()).filter(|n| *n > 0);
    let req = Request { method: client::Method::Get, url, headers, body: None, range_from: have };
    struct State {
        writer: Option<Box<dyn WriteFile>>,
        done: u64,
        total: Option<u64>,
        last_report: u32,
    }
    let st = RefCell::new(State { writer: None, done: have.unwrap_or(0), total: None, last_report: now_ms() });
    let mut on_head = |h: &Head| -> Result<(), String> {
        let mut s = st.borrow_mut();
        let resumed = have.is_some() && h.range_start == have;
        // A partial reply from any other offset cannot be spliced onto the file.
        if h.status == 206 && !resumed {
            return Err(String::from("The server answered with the wrong range."));
        }
        let start = if resumed { have.unwrap_or(0) } else { 0 };
        s.total = h.content_length.map(|len| len + start);
        s.done = start;
        s.writer = Some(if resumed { fs.append(&part) } else { fs.create(&part) }.map_err(|e| alloc::format!("card: {e:?}"))?);
        Ok(())
    };
    let mut sink = |chunk: &[u8]| -> Result<bool, String> {
        if cancel() {
            return Err(String::from("cancelled"));
        }
        let mut s = st.borrow_mut();
        let w = s.writer.as_mut().ok_or_else(|| String::from("no file"))?;
        w.write_all(chunk).map_err(|e| match e {
            quire_fs::FsError::Full => String::from("card full"),
            other => alloc::format!("card: {other:?}"),
        })?;
        s.done += chunk.len() as u64;
        let now = now_ms();
        if now.wrapping_sub(s.last_report) >= 700 {
            s.last_report = now;
            let (done, total) = (s.done, s.total);
            drop(s);
            progress(done, total);
            crate::touch(now);
        }
        Ok(true)
    };
    let result = client::fetch(stack, &req, &mut on_head, &mut sink).await;
    let (flushed, done, total) = {
        let mut s = st.borrow_mut();
        (s.writer.take().map(|mut w| w.flush().map_err(|e| alloc::format!("card: {e:?}"))), s.done, s.total)
    };
    let head = result?;
    if let Some(Err(e)) = flushed {
        return Err(Error::Sink(e));
    }
    if let Some(t) = total {
        if done < t {
            return Err(Error::Io);
        }
    }
    if fs.exists(path) {
        let _ = fs.remove(path);
    }
    fs.rename(&part, path).map_err(|e| Error::Sink(alloc::format!("card: {e:?}")))?;
    progress(done, total.or(Some(done)));
    Ok(head)
}

/// A card-safe file name: FAT-safe characters, no leading dots, 80 bytes at most.
fn safe_name(s: &str) -> String {
    let mut out: String = s
        .chars()
        .filter(|c| !c.is_control() && !"\\/:*?\"<>|".contains(*c))
        .collect::<String>()
        .trim()
        .trim_start_matches('.')
        .chars()
        .take(80)
        .collect();
    while out.ends_with(['.', ' ']) {
        out.pop();
    }
    out
}

/// The extension a book gets, from its response.
fn book_extension(head: &Head, url: &str) -> String {
    let from_name = |n: &str| {
        let e = quire_fs::extension(n);
        (!e.is_empty() && e.len() <= 5 && e.bytes().all(|b| b.is_ascii_alphanumeric())).then_some(e)
    };
    if let Some(e) = head.file_name.as_deref().and_then(from_name) {
        return e;
    }
    let ct = head.content_type.as_str();
    if ct.contains("epub") {
        return String::from("epub");
    }
    if ct.contains("fictionbook") || ct.contains("fb2") {
        return String::from("fb2");
    }
    if ct.starts_with("text/plain") {
        return String::from("txt");
    }
    if ct.starts_with("text/html") {
        return String::from("html");
    }
    if ct.contains("cbz") {
        return String::from("cbz");
    }
    let name = crate::proto::url::file_name(&head.url);
    from_name(&name).or_else(|| from_name(&crate::proto::url::file_name(url))).unwrap_or_else(|| String::from("epub"))
}

// ---------------------------------------------------------------------------------------
// Books

async fn run_downloads(stack: Stack<'_>, fs: &'static dyn CardFs) {
    while let Some((title, author, url, size)) = crate::next_queued() {
        crate::download_state(&title, DownloadState::Working);
        try_post(crate::downloads_event_msg());
        let result = download_book(stack, fs, &title, &author, &url, size).await;
        match result {
            Ok(()) => {
                crate::transfer_finished(&title, Ok(()));
                post(crate::downloads_event_msg()).await;
                post(NetToMain::Ui(Event::BooksChanged)).await;
            }
            Err(e) => {
                crate::transfer_finished(&title, Err(e));
                post(crate::downloads_event_msg()).await;
            }
        }
    }
    ui(NetEvent::Downloads).await;
}

async fn download_book(
    stack: Stack<'_>,
    fs: &'static dyn CardFs,
    title: &str,
    author: &str,
    url: &str,
    size: Option<u64>,
) -> Result<(), String> {
    let stem = {
        let mut s = safe_name(title);
        if s.is_empty() {
            s = safe_name(&crate::proto::url::file_name(url));
        }
        if s.is_empty() {
            s = String::from("book");
        }
        if !author.is_empty() {
            let a = safe_name(author);
            if !a.is_empty() && s.len() + a.len() < 90 {
                s = alloc::format!("{s} - {a}");
            }
        }
        s
    };
    // Download under a temporary name; the extension is known once the reply is in.
    let tmp = alloc::format!("{}/{stem}.download", crate::proto::routes::BOOKS_DIR);
    crate::transfer_started(title, url, size, 0);
    with(|i| {
        if let Some(d) = i.downloads.iter_mut().find(|d| d.title == title) {
            d.author = String::from(author);
        }
    });
    let mut tries = 0u8;
    loop {
        tries += 1;
        let t2 = String::from(title);
        let t3 = String::from(title);
        let mut progress = |done: u64, total: Option<u64>| {
            crate::transfer_progress(&t2, done);
            if let Some(t) = total {
                with(|i| {
                    if let Some(d) = i.downloads.iter_mut().find(|d| d.title == t2) {
                        d.total = Some(t);
                    }
                });
            }
            try_post(crate::downloads_event_msg());
        };
        let mut cancel = || crate::cancelled(&t3);
        match download_file(stack, url, &tmp, &[("Accept", "application/epub+zip, */*")], fs, &mut progress, &mut cancel).await {
            Ok(head) => {
                let ext = book_extension(&head, url);
                let mut path = alloc::format!("{}/{stem}.{ext}", crate::proto::routes::BOOKS_DIR);
                if fs.exists(&path) {
                    path = alloc::format!("{}/{stem} ({}).{ext}", crate::proto::routes::BOOKS_DIR, now_ms() % 1000);
                }
                fs.rename(&tmp, &path).map_err(|e| alloc::format!("card: {e:?}"))?;
                log::info!("download: {path}");
                return Ok(());
            }
            Err(Error::Status(s)) if matches!(s, 429 | 503) && tries < DOWNLOAD_TRIES => {
                log::info!("download: {s}, retry {tries}/{DOWNLOAD_TRIES}");
                for left in (1..=RETRY_WAIT_S).rev() {
                    if crate::cancelled(title) {
                        return Err(String::from("cancelled"));
                    }
                    crate::download_state(title, DownloadState::Retrying(left));
                    try_post(crate::downloads_event_msg());
                    Timer::after(Duration::from_secs(1)).await;
                }
                crate::download_state(title, DownloadState::Working);
            }
            Err(e) => return Err(e.text(url)),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Shelves, OPDS

async fn shelves(stack: Stack<'_>, fs: &'static dyn CardFs) {
    let tmp = alloc::format!("{CATALOG_FILE}.new");
    let r = download_file(stack, CATALOG_URL, &tmp, &[], fs, &mut |_, _| {}, &mut || false).await;
    match r {
        Ok(_) => {
            // Only a catalog the screens can read replaces the old one.
            let ok = fs.open(&tmp).ok().and_then(|f| CatalogFile::parse(crate::fs::DynFile(f))).is_some_and(|c| c.count() > 0);
            if ok {
                if fs.exists(CATALOG_FILE) {
                    let _ = fs.remove(CATALOG_FILE);
                }
                match fs.rename(&tmp, CATALOG_FILE) {
                    Ok(()) => log::info!("shelves: catalog updated"),
                    Err(e) => log::warn!("shelves: rename {e:?}"),
                }
            } else {
                log::warn!("shelves: not a catalog file");
                let _ = fs.remove(&tmp);
            }
        }
        Err(e) => log::warn!("shelves: {}", e.text(CATALOG_URL)),
    }
    ui(NetEvent::Shelves).await;
}

async fn opds_feed(stack: Stack<'_>, url: String) {
    let mut parser = opds::FeedParser::new(&url);
    let json: RefCell<Option<Vec<u8>>> = RefCell::new(None);
    let mut sink = |chunk: &[u8]| -> Result<bool, String> {
        match json.borrow_mut().as_mut() {
            Some(j) => {
                let room = OPDS_JSON_MAX.saturating_sub(j.len());
                j.extend_from_slice(&chunk[..chunk.len().min(room)]);
                Ok(room > chunk.len())
            }
            None => {
                parser.push(chunk);
                Ok(true)
            }
        }
    };
    let mut on_head = |h: &Head| -> Result<(), String> {
        if h.content_type.contains("json") {
            *json.borrow_mut() = Some(Vec::new());
        }
        Ok(())
    };
    let req = Request {
        headers: &[("Accept", "application/atom+xml;profile=opds-catalog, application/opds+json, application/atom+xml, */*")],
        ..Request::get(&url)
    };
    let r = client::fetch(stack, &req, &mut on_head, &mut sink).await;
    let ev = match r {
        Ok(_) => match json.into_inner() {
            Some(j) => opds::parse_json(&url, &j).ok_or_else(|| String::from("Not an OPDS catalog.")),
            None => {
                let (title, entries) = parser.finish();
                if entries.is_empty() && title == "Catalog" {
                    Err(String::from("Not an OPDS catalog."))
                } else {
                    Ok((title, entries))
                }
            }
        },
        Err(e) => Err(e.text(&url)),
    };
    ui(NetEvent::Opds(ev)).await;
}

// ---------------------------------------------------------------------------------------
// Wikipedia, weather, news

async fn wikipedia(stack: Stack<'_>, fs: &'static dyn CardFs, term: String) {
    let lang = Settings::load(&DynFs(fs)).language;
    let url = wiki::summary_url(&lang, &term);
    let req = Request { headers: &[("Accept", "application/json")], ..Request::get(&url) };
    let ev = match client::fetch_to_vec(stack, &req, WIKI_MAX).await {
        Ok((_, body)) => wiki::parse_summary(&body),
        Err(Error::Status(404)) => Err(String::from("No article with that name.")),
        Err(e) => Err(e.text(&url)),
    };
    ui(NetEvent::Wikipedia(ev)).await;
}

async fn weather_job(stack: Stack<'_>, fs: &'static dyn CardFs) {
    let dfs = DynFs(fs);
    let mut settings = Settings::load(&dfs);
    if settings.weather_place.trim().is_empty() {
        log::info!("weather: no place set");
        ui(NetEvent::Weather).await;
        return;
    }
    if settings.weather_lat == 0 && settings.weather_lon == 0 {
        let url = weather::geocode_url(&settings.weather_place);
        match client::fetch_to_vec(stack, &Request::get(&url), WEATHER_MAX).await {
            Ok((_, body)) => match weather::parse_geocode(&body) {
                Some((lat, lon, name)) => {
                    log::info!("weather: {} is {name} at {lat},{lon}", settings.weather_place);
                    settings.weather_lat = lat;
                    settings.weather_lon = lon;
                    if settings.save(&dfs).is_ok() {
                        post(NetToMain::SettingsChanged).await;
                    }
                }
                None => {
                    log::warn!("weather: place not found");
                    ui(NetEvent::Weather).await;
                    return;
                }
            },
            Err(e) => {
                log::warn!("weather: {}", e.text(&url));
                ui(NetEvent::Weather).await;
                return;
            }
        }
    }
    let url = weather::forecast_url(settings.weather_lat, settings.weather_lon);
    match client::fetch_to_vec(stack, &Request::get(&url), WEATHER_MAX).await {
        Ok((_, body)) => match weather::parse_forecast(&body) {
            Some(report) => with(|i| i.weather = Some(report)),
            None => log::warn!("weather: unreadable forecast"),
        },
        Err(e) => log::warn!("weather: {}", e.text(&url)),
    }
    ui(NetEvent::Weather).await;
}

async fn news_job(stack: Stack<'_>, fs: &'static dyn CardFs) {
    let dfs = DynFs(fs);
    let feeds_list = Settings::load(&dfs).news_feeds;
    let _ = Fs::mkdir_all(&dfs, news::NEWS_DIR);
    for feed_url in feeds_list.iter().take(32) {
        let known: Vec<String> = news::load_index(&dfs).into_iter().map(|m| m.url).collect();
        let mut metas: Vec<ArticleMeta> = Vec::new();
        let mut on_article = |a: news::Article| {
            if !known.contains(&a.url) {
                // The body goes to the card at once; only the metadata waits for the index.
                let _ = dfs.write_atomic(&news::body_path(&a.url), a.text.as_bytes());
            }
            metas.push(ArticleMeta {
                feed: a.feed,
                feed_title: a.feed_title,
                title: a.title,
                url: a.url,
                published: a.published,
                read: false,
            });
        };
        let mut parser = feeds::FeedParser::new(feed_url, &mut on_article);
        let mut sink = |chunk: &[u8]| -> Result<bool, String> { Ok(parser.push(chunk)) };
        let req = Request {
            headers: &[("Accept", "application/rss+xml, application/atom+xml, application/xml, text/xml, */*")],
            ..Request::get(feed_url)
        };
        match client::fetch(stack, &req, &mut |_| Ok(()), &mut sink).await {
            Ok(_) => parser.finish(),
            Err(e) => log::warn!("news {feed_url}: {}", e.text(feed_url)),
        }
        drop(parser);
        if !metas.is_empty() {
            merge_articles(&dfs, metas);
            ui(NetEvent::News).await;
        }
    }
    ui(NetEvent::News).await;
}

/// Merge fetched metadata into the index the news screen reads (bodies were written
/// already): read flags of known articles survive, the newest `MAX_ARTICLES` stay.
fn merge_articles(dfs: &DynFs<'_>, fetched: Vec<ArticleMeta>) {
    let mut index = news::load_index(dfs);
    for a in fetched {
        match index.iter_mut().find(|m| m.url == a.url) {
            Some(m) => {
                m.title = a.title;
                m.feed_title = a.feed_title;
                if a.published != 0 {
                    m.published = a.published;
                }
            }
            None => index.push(a),
        }
    }
    index.sort_by_key(|a| core::cmp::Reverse(a.published));
    for dropped in index.iter().skip(news::MAX_ARTICLES) {
        let _ = Fs::remove(dfs, &dropped.body_path());
    }
    index.truncate(news::MAX_ARTICLES);
    news::save_index(dfs, &index);
}

// ---------------------------------------------------------------------------------------
// Sleep packs

fn packs_root(fs: &dyn CardFs) -> String {
    quire_ui::sleeppack::packs_dir(&Settings::load(&DynFs(fs)).sleep_folder)
}

async fn sleep_packs(stack: Stack<'_>, fs: &'static dyn CardFs) {
    match client::fetch_to_vec(stack, &Request::get(sleepidx::INDEX_URL), sleepidx::MAX_JSON).await {
        Ok((_, body)) => {
            let root = packs_root(fs);
            let rows: Vec<(String, String, u16, u64, bool)> = sleepidx::parse_index(&body)
                .into_iter()
                .map(|(id, name, desc, images, bytes)| {
                    let installed = fs.exists(&alloc::format!("{root}/{id}/{}", quire_ui::sleeppack::PACK_FILE));
                    (id, alloc::format!("{name} — {desc}"), images, bytes, installed)
                })
                .collect();
            with(|i| i.sleep_packs = rows);
        }
        Err(e) => log::warn!("sleep packs: {}", e.text(sleepidx::INDEX_URL)),
    }
    ui(NetEvent::SleepPacks).await;
}

async fn sleep_pack(stack: Stack<'_>, fs: &'static dyn CardFs, id: String) {
    let title = alloc::format!("Sleep pack {id}");
    let manifest_url = sleepidx::pack_file_url(&id, quire_ui::sleeppack::PACK_FILE);
    crate::transfer_started(&title, &manifest_url, None, 0);
    try_post(crate::downloads_event_msg());
    let result: Result<(), String> = async {
        let (_, manifest) =
            client::fetch_to_vec(stack, &Request::get(&manifest_url), sleepidx::MAX_JSON).await.map_err(|e| e.text(&manifest_url))?;
        let files = sleepidx::parse_pack_files(&manifest);
        if files.is_empty() {
            return Err(String::from("The pack has no images."));
        }
        let dir = alloc::format!("{}/{id}", packs_root(fs));
        fs.mkdir_all(&dir).map_err(|e| alloc::format!("card: {e:?}"))?;
        let n = files.len() as u64;
        for (k, file) in files.iter().enumerate() {
            let url = sleepidx::pack_file_url(&id, file);
            let path = alloc::format!("{dir}/{file}");
            let t2 = title.clone();
            let mut progress = |_done: u64, _total: Option<u64>| {
                crate::transfer_progress(&t2, k as u64);
                try_post(crate::downloads_event_msg());
            };
            let t3 = title.clone();
            let mut cancel = || crate::cancelled(&t3);
            with(|i| {
                if let Some(d) = i.downloads.iter_mut().find(|d| d.title == title) {
                    d.total = Some(n);
                }
            });
            download_file(stack, &url, &path, &[], fs, &mut progress, &mut cancel).await.map_err(|e| e.text(&url))?;
        }
        // The manifest last: a pack is only listed once its images are all there.
        DynFs(fs)
            .write_atomic(&alloc::format!("{dir}/{}", quire_ui::sleeppack::PACK_FILE), &manifest)
            .map_err(|e| alloc::format!("card: {e:?}"))?;
        Ok(())
    }
    .await;
    if let Err(e) = &result {
        log::warn!("sleep pack {id}: {e}");
    }
    crate::transfer_finished(&title, result);
    with(|i| {
        if let Some(p) = i.sleep_packs.iter_mut().find(|p| p.0 == id) {
            p.4 = fs.exists(&alloc::format!("{}/{id}/{}", packs_root(fs), quire_ui::sleeppack::PACK_FILE));
        }
    });
    post(crate::downloads_event_msg()).await;
    ui(NetEvent::SleepPacks).await;
}

// ---------------------------------------------------------------------------------------
// Updates

const GITHUB_HEADERS: [(&str, &str); 2] = [("Accept", "application/vnd.github+json"), ("X-GitHub-Api-Version", "2022-11-28")];

async fn ota_check(stack: Stack<'_>) {
    let req = Request { headers: &GITHUB_HEADERS, ..Request::get(github::LATEST_URL) };
    let ev = match client::fetch_to_vec(stack, &req, github::MAX_JSON).await {
        Ok((_, body)) => match github::parse_release(&body) {
            Ok(rel) => {
                let current = with(|i| i.status.version.clone());
                let newer = github::is_newer(&rel.info.version, &current);
                log::info!("ota: latest {} (running {current}), newer: {newer}", rel.info.version);
                let info: OtaInfo = rel.info.clone();
                critical_section::with(|cs| *LAST_RELEASE.borrow_ref_mut(cs) = Some(rel));
                Ok(newer.then_some(info))
            }
            Err(e) => Err(e),
        },
        Err(Error::Status(404)) => Ok(None),
        Err(e) => Err(e.text(github::LATEST_URL)),
    };
    ui(NetEvent::Ota(ev)).await;
}

/// Fetch the update image to the card, check it against the release's checksum file
/// when one exists, and report `finished: Some(Ok(()))`: the main loop then installs it
/// from the card (`quire_board::ota::install_from_card`).
async fn ota_download(stack: Stack<'_>, fs: &'static dyn CardFs, source: String) {
    if !source.starts_with("http") {
        ui(NetEvent::OtaProgress { done: 0, total: 0, finished: Some(Err(String::from("Card updates install from the main loop."))) })
            .await;
        return;
    }
    with(|i| i.ota_busy = true);
    let mut last = (0u64, 0u64);
    let result: Result<(), String> = async {
        let mut progress = |done: u64, total: Option<u64>| {
            last = (done, total.unwrap_or(0));
            try_post(NetToMain::Ui(Event::Net(NetEvent::OtaProgress { done, total: total.unwrap_or(0), finished: None })));
        };
        download_file(stack, &source, UPDATE_FILE, &[("Accept", "application/octet-stream")], fs, &mut progress, &mut || false)
            .await
            .map_err(|e| e.text(&source))?;
        // The checksum file, when the release publishes one.
        let sha_url = critical_section::with(|cs| LAST_RELEASE.borrow_ref(cs).as_ref().and_then(|r| r.sha_url.clone()));
        let sha_url = sha_url.unwrap_or_else(|| alloc::format!("{source}.sha256"));
        match client::fetch_to_vec(stack, &Request::get(&sha_url), 4096).await {
            Ok((_, text)) => {
                let text = String::from_utf8_lossy(&text);
                let Some(expected) = github::sha_for(&text, github::ASSET) else {
                    return Err(String::from("The checksum file is unreadable."));
                };
                let got = sha256_of(fs, UPDATE_FILE).ok_or_else(|| String::from("Could not read the image back."))?;
                if got != expected {
                    let _ = fs.remove(UPDATE_FILE);
                    return Err(String::from("The image's checksum does not match the release."));
                }
                log::info!("ota: checksum verified");
            }
            Err(Error::Status(404)) => log::info!("ota: no checksum file published"),
            Err(e) => return Err(alloc::format!("Checksum: {}", e.text(&sha_url))),
        }
        Ok(())
    }
    .await;
    with(|i| i.ota_busy = false);
    ui(NetEvent::OtaProgress { done: last.0, total: last.1, finished: Some(result) }).await;
}

/// SHA-256 of a card file, read in 4 KB pieces.
fn sha256_of(fs: &dyn CardFs, path: &str) -> Option<[u8; 32]> {
    use sha2::Digest;
    let file = fs.open(path).ok()?;
    let len = file.len();
    let mut h = sha2::Sha256::new();
    let mut buf = alloc::vec![0u8; 4096];
    let mut off = 0u64;
    while off < len {
        let n = file.read_at(off, &mut buf).ok()?;
        if n == 0 {
            return None;
        }
        h.update(&buf[..n]);
        off += n as u64;
    }
    Some(h.finalize().into())
}
