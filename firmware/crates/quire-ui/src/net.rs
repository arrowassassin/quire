//! The UI's view of the network layer: jobs it can ask for and the state it reads.
//! The device implements [`NetState`] in the net task; the simulator fakes it.

use alloc::string::String;
use alloc::vec::Vec;
use quire_library::BookId;

/// Something to fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchRequest {
    /// Download a book from a URL into the library.
    Book {
        /// Source URL.
        url: String,
        /// Title for the queue.
        title: String,
        /// Author line.
        author: String,
        /// Expected size, if known.
        size: Option<u64>,
    },
    /// Fetch an OPDS feed.
    Opds(String),
    /// Refresh the Bookshop shelves.
    Shelves,
    /// Fetch a dictionary or Wikipedia summary for a word.
    Wikipedia(String),
    /// Weather for the configured place.
    Weather,
    /// News feeds.
    News,
    /// Sleep image pack index.
    SleepPacks,
    /// Install a sleep image pack by name.
    SleepPack(String),
    /// Check for a firmware update.
    OtaCheck,
    /// Cancel a download, named by its URL (for an upload, its card path).
    ///
    /// Not by its position: the request crosses to the net task, and the list there
    /// can be reordered in the meantime — a transfer starting moves to the end — so a
    /// position taken from the screen's copy can name a different download by the
    /// time it is acted on.
    Cancel(String),
    /// Retry a failed download, named by its URL.
    Retry(String),
}

/// A queued or finished download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Download {
    /// Title.
    pub title: String,
    /// Author.
    pub author: String,
    /// Source URL.
    pub url: String,
    /// Bytes received.
    pub done: u64,
    /// Total bytes, if known.
    pub total: Option<u64>,
    /// State.
    pub state: DownloadState,
    /// The library id once added.
    pub book: Option<BookId>,
}

/// Download state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadState {
    /// Waiting.
    Queued,
    /// Transferring.
    Working,
    /// Done and in the library.
    Done,
    /// Failed with a message.
    Failed(String),
    /// Waiting for a retry ("Gutenberg is limiting requests, retrying in 30 s").
    Retrying(u16),
}

/// An OPDS catalog entry.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct OpdsEntry {
    /// Title.
    pub title: String,
    /// Author.
    pub author: String,
    /// Navigation link (a sub-catalog) or none.
    pub nav: Option<String>,
    /// Acquisition link (a book file) or none.
    pub acquisition: Option<String>,
    /// Summary.
    pub summary: String,
}

/// An available firmware update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtaInfo {
    /// Version string.
    pub version: String,
    /// Release notes.
    pub notes: String,
    /// Download URL.
    pub url: String,
    /// Size bytes.
    pub size: u64,
}

/// Events from the network layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetEvent {
    /// The download queue changed.
    Downloads,
    /// An OPDS feed arrived (or failed).
    Opds(Result<(String, Vec<OpdsEntry>), String>),
    /// Shelves refreshed.
    Shelves,
    /// Wikipedia summary for a word.
    Wikipedia(Result<(String, String), String>),
    /// Weather arrived.
    Weather,
    /// News refreshed.
    News,
    /// OTA check finished.
    Ota(Result<Option<OtaInfo>, String>),
    /// OTA progress (bytes done, total) or finished.
    OtaProgress {
        /// Bytes.
        done: u64,
        /// Total.
        total: u64,
        /// Finished with Ok or an error.
        finished: Option<Result<(), String>>,
    },
    /// A Calibre transfer began or ended.
    Calibre(String),
    /// Sync finished.
    Sync(Result<u32, String>),
    /// Sleep packs list arrived.
    SleepPacks,
}

/// A weather report: temperature °C, condition, and five (day, high, low).
pub type WeatherReport = (i16, String, Vec<(String, i16, i16)>);

/// Network state the screens read.
pub trait NetState {
    /// The download queue.
    fn downloads(&self) -> &[Download];
    /// The last weather report.
    fn weather(&self) -> Option<WeatherReport>;
    /// Calibre server status line.
    fn calibre_status(&self) -> String;
    /// Whether an OTA is in progress.
    fn ota_busy(&self) -> bool;
    /// Available sleep image packs: (name, description, images, bytes, installed).
    fn sleep_packs(&self) -> Vec<(String, String, u16, u64, bool)>;
}

/// A network state that has nothing (Read mode).
#[derive(Default)]
pub struct NoNet {
    downloads: Vec<Download>,
}

impl NetState for NoNet {
    fn downloads(&self) -> &[Download] {
        &self.downloads
    }
    fn weather(&self) -> Option<WeatherReport> {
        None
    }
    fn calibre_status(&self) -> String {
        String::from("Wi-Fi off")
    }
    fn ota_busy(&self) -> bool {
        false
    }
    fn sleep_packs(&self) -> Vec<(String, String, u16, u64, bool)> {
        Vec::new()
    }
}
