//! The URL routing table of the Drop page server, and the path rules for files.

use alloc::string::String;

/// Where a request goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// The Drop page (also served at `/type`, which opens the Window tab).
    Index,
    /// The captive landing page.
    Captive,
    /// An OS captive-portal probe: redirect to `/captive`.
    CaptiveProbe,
    /// Apple's reachability probe, answered with the success page it wants.
    AppleProbe,
    /// PWA manifest.
    Manifest,
    /// Service worker.
    ServiceWorker,
    /// App icon.
    Icon,
    /// `GET /api/status`.
    Status,
    /// `GET /api/library`.
    Library,
    /// `GET /api/files/<path>`: download.
    FileGet(String),
    /// `PUT /api/files/<path>`: upload (resumable).
    FilePut(String),
    /// `DELETE /api/files/<path>`.
    FileDelete(String),
    /// `POST /api/files/<path>/rename`.
    FileRename(String),
    /// `POST /upload`: multipart fallback.
    Upload,
    /// `GET /api/settings`.
    SettingsGet,
    /// `PUT /api/settings`.
    SettingsPut,
    /// `GET /api/stats`.
    Stats,
    /// `GET /api/screen.pbm`.
    Screen,
    /// `GET /ws`.
    Ws,
    /// `POST /sleep?name=`: a dithered sleep image.
    Sleep,
    /// `POST /api/fetch`: ask the client-side fetchers for something.
    Fetch,
    /// Unknown path.
    NotFound,
    /// Known path, wrong method.
    MethodNotAllowed,
}

impl Route {
    /// Whether the route changes the card or the device (PIN-gated).
    pub fn is_write(&self) -> bool {
        matches!(
            self,
            Route::FilePut(_)
                | Route::FileDelete(_)
                | Route::FileRename(_)
                | Route::Upload
                | Route::SettingsPut
                | Route::Sleep
                | Route::Fetch
        )
    }
}

/// Paths the OSes probe to detect a captive portal.
const PROBES: &[&str] = &[
    "/generate_204",
    "/gen_204",
    "/connecttest.txt",
    "/ncsi.txt",
    "/redirect",
    "/success.txt",
    "/canonical.html",
    "/check_network_status.txt",
    "/mobile/status.php",
];

/// The paths iOS and macOS probe, answered with the page they are looking for.
///
/// Anything else — a redirect to the portal, which is what the other probes get
/// — tells them the network is behind a sign-in, and once that sheet is closed
/// they treat a network with no route to the internet as a bad one and leave for
/// a better one, mid-transfer. The reader is the whole point of this network, so
/// it says what keeps them on it.
const APPLE_PROBES: &[&str] = &["/hotspot-detect.html", "/library/test/success.html"];

/// What macOS and iOS expect from their probe, byte for byte.
pub const APPLE_SUCCESS: &str = "<HTML><HEAD><TITLE>Success</TITLE></HEAD><BODY>Success</BODY></HTML>\n";

/// Route `method` + `path` (the path as sent, percent-encoded, without the query).
pub fn route(method: &str, path: &str) -> Route {
    let m = method.to_ascii_uppercase();
    let get = m == "GET" || m == "HEAD";
    match path {
        "/" | "/index.html" | "/type" => {
            return if get { Route::Index } else { Route::MethodNotAllowed };
        }
        "/captive" => return if get { Route::Captive } else { Route::MethodNotAllowed },
        "/manifest.webmanifest" => return if get { Route::Manifest } else { Route::MethodNotAllowed },
        "/sw.js" => return if get { Route::ServiceWorker } else { Route::MethodNotAllowed },
        "/icon.svg" => return if get { Route::Icon } else { Route::MethodNotAllowed },
        "/ws" => return if get { Route::Ws } else { Route::MethodNotAllowed },
        "/upload" => return if m == "POST" { Route::Upload } else { Route::MethodNotAllowed },
        "/sleep" => return if m == "POST" { Route::Sleep } else { Route::MethodNotAllowed },
        "/api/status" => return if get { Route::Status } else { Route::MethodNotAllowed },
        "/api/library" => return if get { Route::Library } else { Route::MethodNotAllowed },
        "/api/stats" => return if get { Route::Stats } else { Route::MethodNotAllowed },
        "/api/screen.pbm" => return if get { Route::Screen } else { Route::MethodNotAllowed },
        "/api/fetch" => return if m == "POST" { Route::Fetch } else { Route::MethodNotAllowed },
        "/api/settings" => {
            return match m.as_str() {
                "GET" | "HEAD" => Route::SettingsGet,
                "PUT" | "POST" | "PATCH" => Route::SettingsPut,
                _ => Route::MethodNotAllowed,
            };
        }
        _ => {}
    }
    if let Some(rest) = path.strip_prefix("/api/files/") {
        let decoded = quire_fs::percent_decode(rest);
        if m == "POST" {
            if let Some(p) = decoded.strip_suffix("/rename") {
                return match sanitize_path(p) {
                    Some(p) => Route::FileRename(p),
                    None => Route::NotFound,
                };
            }
            return Route::MethodNotAllowed;
        }
        let Some(p) = sanitize_path(&decoded) else { return Route::NotFound };
        return match m.as_str() {
            "GET" | "HEAD" => Route::FileGet(p),
            "PUT" => Route::FilePut(p),
            "DELETE" => Route::FileDelete(p),
            _ => Route::MethodNotAllowed,
        };
    }
    if APPLE_PROBES.contains(&path) {
        return if get { Route::AppleProbe } else { Route::MethodNotAllowed };
    }
    if PROBES.contains(&path) {
        return Route::CaptiveProbe;
    }
    Route::NotFound
}

/// Directory files land in when a request gives a bare name.
pub const BOOKS_DIR: &str = "/Books";

/// Normalise a card path from a request: absolute, no `..` or empty segments, no
/// control characters or characters FAT cannot store; a bare file name goes to `/Books`.
/// Paths under `/.quire` (the cache) are refused.
pub fn sanitize_path(p: &str) -> Option<String> {
    let p = p.trim();
    if p.is_empty() || p.len() > 255 {
        return None;
    }
    let mut out = String::with_capacity(p.len() + 8);
    let segs: alloc::vec::Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return None;
    }
    if segs.len() == 1 && !p.starts_with('/') {
        out.push_str(BOOKS_DIR);
    }
    for s in &segs {
        if *s == "." || *s == ".." || s.ends_with('.') || s.ends_with(' ') {
            return None;
        }
        if s.bytes().any(|b| b < 0x20 || b == 0x7f || b"\\:*?\"<>|".contains(&b)) {
            return None;
        }
        out.push('/');
        out.push_str(s);
    }
    if out == "/.quire" || out.starts_with("/.quire/") {
        return None;
    }
    Some(out)
}

/// The value of `name` in a query string, percent-decoded.
pub fn query_param(query: Option<&str>, name: &str) -> Option<String> {
    let q = query?;
    for pair in q.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == name {
            return Some(quire_fs::percent_decode(&v.replace('+', " ")));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table() {
        assert_eq!(route("GET", "/"), Route::Index);
        assert_eq!(route("GET", "/type"), Route::Index);
        assert_eq!(route("HEAD", "/api/status"), Route::Status);
        assert_eq!(route("POST", "/"), Route::MethodNotAllowed);
        assert_eq!(route("GET", "/ws"), Route::Ws);
        assert_eq!(route("PUT", "/api/settings"), Route::SettingsPut);
        assert_eq!(route("POST", "/upload"), Route::Upload);
        assert_eq!(route("GET", "/upload"), Route::MethodNotAllowed);
        assert_eq!(route("GET", "/generate_204"), Route::CaptiveProbe);
        // Apple's two probes are answered rather than redirected, so that iOS and
        // macOS stay on a network that has nothing beyond the reader.
        assert_eq!(route("GET", "/hotspot-detect.html"), Route::AppleProbe);
        assert_eq!(route("GET", "/library/test/success.html"), Route::AppleProbe);
        assert_eq!(route("POST", "/hotspot-detect.html"), Route::MethodNotAllowed);
        // Byte for byte what they look for: anything else reads as a portal.
        assert!(APPLE_SUCCESS.contains("<TITLE>Success</TITLE>"));
        assert!(APPLE_SUCCESS.contains("<BODY>Success</BODY>"));
        assert_eq!(route("GET", "/nothing"), Route::NotFound);
        assert_eq!(route("POST", "/api/fetch"), Route::Fetch);
    }

    #[test]
    fn files() {
        assert_eq!(route("PUT", "/api/files/Books/a%20b.epub"), Route::FilePut("/Books/a b.epub".into()));
        assert_eq!(route("PUT", "/api/files/a.epub"), Route::FilePut("/Books/a.epub".into()));
        assert_eq!(route("GET", "/api/files/Books/x.txt"), Route::FileGet("/Books/x.txt".into()));
        assert_eq!(route("DELETE", "/api/files/Books/x.txt"), Route::FileDelete("/Books/x.txt".into()));
        assert_eq!(route("POST", "/api/files/Books/x.txt/rename"), Route::FileRename("/Books/x.txt".into()));
        assert_eq!(route("PATCH", "/api/files/Books/x.txt"), Route::MethodNotAllowed);
        assert_eq!(route("PUT", "/api/files/../x"), Route::NotFound);
        assert_eq!(route("PUT", "/api/files/.quire/index.bin"), Route::NotFound);
        assert_eq!(route("PUT", "/api/files/"), Route::NotFound);
        assert!(Route::FilePut("/a".into()).is_write());
        assert!(!Route::FileGet("/a".into()).is_write());
    }

    #[test]
    fn sanitizes() {
        assert_eq!(sanitize_path("a.epub").as_deref(), Some("/Books/a.epub"));
        assert_eq!(sanitize_path("/a.epub").as_deref(), Some("/a.epub"));
        assert_eq!(sanitize_path("Books//sub/b.txt").as_deref(), Some("/Books/sub/b.txt"));
        assert_eq!(sanitize_path("Books/../x"), None);
        assert_eq!(sanitize_path("Books/bad:name"), None);
        assert_eq!(sanitize_path("Books/trail."), None);
        assert_eq!(sanitize_path("Books/nl\nx"), None);
        assert_eq!(sanitize_path(""), None);
        assert_eq!(sanitize_path("/.quire/x"), None);
    }

    #[test]
    fn query() {
        assert_eq!(query_param(Some("name=a%20b.pbm&x=1"), "name").as_deref(), Some("a b.pbm"));
        assert_eq!(query_param(Some("name=c+d"), "name").as_deref(), Some("c d"));
        assert_eq!(query_param(Some("x=1"), "name"), None);
        assert_eq!(query_param(None, "name"), None);
    }
}
