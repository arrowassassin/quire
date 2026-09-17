//! The library index: one small record per book, kept in RAM and written atomically to
//! `/.quire/library.bin`. Positions, status, collections and per-book reading totals
//! live here so the Library, Book info and Analytics screens never touch the cache.

use alloc::string::String;
use alloc::vec::Vec;
use quire_fs::Fs;
use quire_layout::Pos;
use serde::{Deserialize, Serialize};

use crate::{BookId, LibError, LibResult, INDEX_FILE, ROOT};

/// Index file version.
const VERSION: u16 = 1;
/// Upper bound on entries (a card with more files than this is still fine; the rest are
/// reachable through the folder browser). The device keeps the index in RAM at roughly
/// 300 bytes and six allocations a book, so it stops at a modest number.
#[cfg(target_os = "none")]
pub const MAX_BOOKS: usize = 150;
/// The positions file: tiny, rewritten on position changes instead of the whole index.
pub const POSITIONS_FILE: &str = "/.quire/positions.bin";
/// Longest error message kept in an entry.
const ERROR_BYTES: usize = 48;
/// Upper bound on entries on the host.
#[cfg(not(target_os = "none"))]
pub const MAX_BOOKS: usize = 4000;

/// A position in a book that survives font changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Loc {
    /// Cache section (chapter file) index.
    pub section: u16,
    /// Exact position within the section.
    pub pos: Pos,
    /// Characters before `pos` in the whole book (for percent and time-left).
    pub chars: u32,
}

/// Reading status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Status {
    /// Never opened.
    #[default]
    Unread,
    /// Opened at least once.
    Reading,
    /// Marked or detected finished.
    Finished,
    /// Put aside deliberately.
    Abandoned,
}

/// Whether the book's cache is ready.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum IngestState {
    /// Seen on the card, not yet converted.
    #[default]
    Pending,
    /// Chapter cache complete.
    Ready,
    /// Conversion failed (the message is in `error`).
    Failed,
}

/// Per-book reading totals, updated when a session ends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BookStats {
    /// Seconds read.
    pub seconds: u32,
    /// Pages turned.
    pub pages: u32,
    /// Sessions.
    pub sessions: u16,
    /// Day (local, days since epoch) first read.
    pub started: Option<u16>,
    /// Day finished.
    pub finished: Option<u16>,
    /// Last seven sessions' (characters, seconds) for pace forecasts.
    pub recent: Vec<(u32, u32)>,
    /// Rating 0–5.
    pub rating: u8,
}

/// One book.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookEntry {
    /// Stable content id; also the cache directory name.
    pub id: BookId,
    /// Path of the source file on the card.
    pub path: String,
    /// Source size in bytes.
    pub size: u64,
    /// Detected format.
    pub format: quire_doc::Format,
    /// Display title (from metadata or the file name).
    pub title: String,
    /// Authors.
    pub authors: Vec<String>,
    /// Series and index × 10.
    pub series: Option<(String, u16)>,
    /// Year.
    pub year: Option<u16>,
    /// Language code.
    pub language: String,
    /// Number of cache sections.
    pub sections: u16,
    /// Total characters.
    pub chars: u32,
    /// Whether a cover was extracted.
    pub has_cover: bool,
    /// Cache state.
    pub ingest: IngestState,
    /// Ingest error text, if any.
    pub error: Option<String>,
    /// When the file was first seen (local seconds).
    pub added: u32,
    /// When it was last opened.
    pub last_opened: u32,
    /// Where the reader is.
    pub loc: Loc,
    /// Reading status.
    pub status: Status,
    /// Collections this book belongs to.
    pub collections: Vec<u16>,
    /// Reading totals.
    pub stats: BookStats,
    /// True when the source file is no longer on the card.
    pub missing: bool,
    /// Total pages for a typography profile key, once its page index is complete.
    pub pages_total: Option<(u32, u32)>,
}

/// The per-book part of the index that changes while reading.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PosRecord {
    id: BookId,
    loc: Loc,
    status: Status,
    last_opened: u32,
    started: Option<u16>,
    finished: Option<u16>,
}

impl BookEntry {
    /// Percent read, 0–100.
    pub fn percent(&self) -> u8 {
        if self.status == Status::Finished {
            return 100;
        }
        if self.chars == 0 {
            return 0;
        }
        ((self.loc.chars as u64 * 100) / self.chars as u64).min(100) as u8
    }
    /// Characters left.
    pub fn chars_left(&self) -> u32 {
        self.chars.saturating_sub(self.loc.chars)
    }
    /// Author line for lists ("Eliot" / "Gal, Eich +12").
    pub fn author_line(&self) -> String {
        match self.authors.len() {
            0 => String::new(),
            1 => self.authors[0].clone(),
            2 => alloc::format!("{} & {}", self.authors[0], self.authors[1]),
            n => alloc::format!("{} +{}", self.authors[0], n - 1),
        }
    }
    /// Virtual entries (news articles, wiki pages) stay off the shelves.
    pub fn hidden(&self) -> bool {
        self.missing || self.path.starts_with("news:") || self.path.starts_with("wiki:")
    }
    /// Sort key for the shelf: last opened, then added.
    pub fn recency(&self) -> u32 {
        self.last_opened.max(self.added)
    }
}

/// A named collection (shelf).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    /// Id referenced by books.
    pub id: u16,
    /// Display name.
    pub name: String,
    /// Sort order.
    pub order: u16,
}

/// The whole index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Library {
    version: u16,
    /// Books.
    pub books: Vec<BookEntry>,
    /// Collections.
    pub collections: Vec<Collection>,
    /// Folders scanned for books.
    pub sources: Vec<String>,
    /// The book shown as home (last opened).
    pub current: Option<BookId>,
    next_collection: u16,
    #[serde(skip)]
    dirty: bool,
    #[serde(skip)]
    dirty_pos: bool,
    /// Bumped on every change, so screens can tell when a cached view is stale.
    #[serde(skip)]
    generation: u32,
}

impl Library {
    /// Load the index, or start empty when missing or unreadable.
    pub fn load<F: Fs>(fs: &F) -> Library {
        let mut lib = match fs.read_to_vec(INDEX_FILE) {
            Ok(bytes) => postcard::from_bytes::<Library>(&bytes).unwrap_or_default(),
            Err(_) => Library::default(),
        };
        if lib.version != VERSION {
            lib = Library::default();
        }
        lib.version = VERSION;
        if lib.sources.is_empty() {
            lib.sources = crate::DEFAULT_SOURCES.iter().map(|s| String::from(*s)).collect();
        }
        // Positions are written separately and more often; they win over the index copy.
        if let Ok(bytes) = fs.read_to_vec(POSITIONS_FILE) {
            if let Ok(recs) = postcard::from_bytes::<Vec<PosRecord>>(&bytes) {
                for r in recs {
                    if let Some(b) = lib.books.iter_mut().find(|b| b.id == r.id) {
                        b.loc = r.loc;
                        b.status = r.status;
                        b.last_opened = b.last_opened.max(r.last_opened);
                        b.stats.started = b.stats.started.or(r.started);
                        b.stats.finished = r.finished.or(b.stats.finished);
                    }
                }
            }
        }
        lib.dirty = false;
        lib.dirty_pos = false;
        lib
    }

    /// Write whatever changed: the positions file (small, frequent) and the index
    /// (larger, only on structural changes).
    pub fn save<F: Fs>(&mut self, fs: &F) -> LibResult<()> {
        if !self.dirty && !self.dirty_pos {
            return Ok(());
        }
        if !fs.exists(ROOT) {
            fs.mkdir_all(ROOT)?;
        }
        if self.dirty {
            let bytes = postcard::to_allocvec(self).map_err(|_| LibError::Corrupt("index encode"))?;
            fs.write_atomic(INDEX_FILE, &bytes)?;
            self.dirty = false;
        }
        if self.dirty_pos {
            self.save_positions(fs)?;
        }
        Ok(())
    }

    /// Write only the positions file (called on page turns' periodic save and before sleep).
    pub fn save_positions<F: Fs>(&mut self, fs: &F) -> LibResult<()> {
        let recs: Vec<PosRecord> = self
            .books
            .iter()
            .filter(|b| b.status != Status::Unread || b.last_opened > 0)
            .map(|b| PosRecord {
                id: b.id,
                loc: b.loc,
                status: b.status,
                last_opened: b.last_opened,
                started: b.stats.started,
                finished: b.stats.finished,
            })
            .collect();
        let bytes = postcard::to_allocvec(&recs).map_err(|_| LibError::Corrupt("positions encode"))?;
        if !fs.exists(ROOT) {
            fs.mkdir_all(ROOT)?;
        }
        fs.write_atomic(POSITIONS_FILE, &bytes)?;
        self.dirty_pos = false;
        Ok(())
    }

    /// A counter bumped on every change to the index or the positions: a screen that
    /// caches a sorted view compares it to know when to rebuild.
    pub fn generation(&self) -> u32 {
        self.generation
    }
    /// Whether there are unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.dirty || self.dirty_pos
    }
    /// Mark the index changed (structure or metadata).
    pub fn touch(&mut self) {
        self.dirty = true;
        self.generation = self.generation.wrapping_add(1);
    }
    /// Mark positions changed.
    pub fn touch_positions(&mut self) {
        self.dirty_pos = true;
        self.generation = self.generation.wrapping_add(1);
    }

    /// Find a book.
    pub fn get(&self, id: BookId) -> Option<&BookEntry> {
        self.books.iter().find(|b| b.id == id)
    }
    /// Find a book mutably. Call [`Library::touch`] (or `touch_positions`) after changing it.
    pub fn get_mut(&mut self, id: BookId) -> Option<&mut BookEntry> {
        self.books.iter_mut().find(|b| b.id == id)
    }
    /// Find by path.
    pub fn by_path(&self, path: &str) -> Option<&BookEntry> {
        // Case-insensitively, because the card is FAT32 and so is not case
        // sensitive either. The default sources list both "/Books" and "/books"
        // to cope with a card written either way; on the card itself those are
        // one directory, so a scan walks it twice and the second pass would
        // otherwise miss the entry it just made, re-hash the file, and rewrite
        // the stored path to the other spelling — leaving every lookup by the
        // path the card actually uses to fail.
        self.books.iter().find(|b| b.path.eq_ignore_ascii_case(path))
    }

    /// Add or update an entry.
    pub fn upsert(&mut self, mut entry: BookEntry) {
        self.dirty = true;
        self.generation = self.generation.wrapping_add(1);
        if let Some(e) = entry.error.as_mut() {
            if e.len() > ERROR_BYTES {
                let mut cut = ERROR_BYTES;
                while !e.is_char_boundary(cut) {
                    cut -= 1;
                }
                e.truncate(cut);
            }
        }
        if let Some(e) = self.books.iter_mut().find(|b| b.id == entry.id) {
            *e = entry;
        } else if self.books.len() < MAX_BOOKS {
            self.books.push(entry);
        }
    }

    /// Remove an entry (the cache is removed separately).
    pub fn remove(&mut self, id: BookId) -> Option<BookEntry> {
        let i = self.books.iter().position(|b| b.id == id)?;
        self.dirty = true;
        self.generation = self.generation.wrapping_add(1);
        if self.current == Some(id) {
            self.current = None;
        }
        Some(self.books.remove(i))
    }

    /// Books ordered for the shelf: current first, then by recency.
    pub fn shelf(&self) -> Vec<&BookEntry> {
        let mut v: Vec<&BookEntry> = self.books.iter().filter(|b| !b.hidden()).collect();
        v.sort_by(|a, b| b.recency().cmp(&a.recency()).then_with(|| a.title.cmp(&b.title)));
        if let Some(cur) = self.current {
            if let Some(i) = v.iter().position(|b| b.id == cur) {
                let b = v.remove(i);
                v.insert(0, b);
            }
        }
        v
    }

    /// Books by title.
    pub fn by_title(&self) -> Vec<&BookEntry> {
        let mut v: Vec<&BookEntry> = self.books.iter().filter(|b| !b.hidden()).collect();
        v.sort_by_key(|b| sort_title(&b.title));
        v
    }

    /// Books by author, then series index, then title.
    pub fn by_author(&self) -> Vec<&BookEntry> {
        let mut v: Vec<&BookEntry> = self.books.iter().filter(|b| !b.hidden()).collect();
        v.sort_by(|a, b| {
            let ka = a.authors.first().map(|s| s.to_lowercase()).unwrap_or_default();
            let kb = b.authors.first().map(|s| s.to_lowercase()).unwrap_or_default();
            ka.cmp(&kb)
                .then_with(|| a.series.as_ref().map(|s| s.1).unwrap_or(0).cmp(&b.series.as_ref().map(|s| s.1).unwrap_or(0)))
                .then_with(|| sort_title(&a.title).cmp(&sort_title(&b.title)))
        });
        v
    }

    /// Books in a collection, by title.
    pub fn in_collection(&self, id: u16) -> Vec<&BookEntry> {
        let mut v: Vec<&BookEntry> = self.books.iter().filter(|b| !b.hidden() && b.collections.contains(&id)).collect();
        v.sort_by_key(|b| sort_title(&b.title));
        v
    }

    /// Books pending ingest, oldest first.
    pub fn pending(&self) -> Vec<BookId> {
        let mut v: Vec<&BookEntry> = self.books.iter().filter(|b| !b.missing && b.ingest == IngestState::Pending).collect();
        v.sort_by_key(|b| b.added);
        v.iter().map(|b| b.id).collect()
    }

    /// Create a collection.
    pub fn add_collection(&mut self, name: &str) -> u16 {
        self.dirty = true;
        self.generation = self.generation.wrapping_add(1);
        self.next_collection = self.next_collection.max(1);
        let id = self.next_collection;
        self.next_collection += 1;
        let order = self.collections.len() as u16;
        self.collections.push(Collection { id, name: name.into(), order });
        id
    }

    /// Rename a collection.
    pub fn rename_collection(&mut self, id: u16, name: &str) {
        if let Some(c) = self.collections.iter_mut().find(|c| c.id == id) {
            c.name = name.into();
            self.dirty = true;
            self.generation = self.generation.wrapping_add(1);
        }
    }

    /// Delete a collection and its memberships.
    pub fn remove_collection(&mut self, id: u16) {
        self.dirty = true;
        self.generation = self.generation.wrapping_add(1);
        self.collections.retain(|c| c.id != id);
        for b in &mut self.books {
            b.collections.retain(|c| *c != id);
        }
    }

    /// Toggle a book's membership.
    pub fn toggle_collection(&mut self, book: BookId, coll: u16) -> bool {
        self.dirty = true;
        self.generation = self.generation.wrapping_add(1);
        if let Some(b) = self.books.iter_mut().find(|b| b.id == book) {
            if let Some(i) = b.collections.iter().position(|c| *c == coll) {
                b.collections.remove(i);
                return false;
            }
            b.collections.push(coll);
            return true;
        }
        false
    }

    /// Books grouped by author (first author), groups sorted by author, books by series then title.
    pub fn authors(&self) -> Vec<(String, Vec<&BookEntry>)> {
        let mut out: Vec<(String, Vec<&BookEntry>)> = Vec::new();
        for b in self.by_author() {
            let name = b.authors.first().cloned().unwrap_or_else(|| String::from("Unknown author"));
            match out.last_mut() {
                Some((n, v)) if n.eq_ignore_ascii_case(&name) => v.push(b),
                _ => out.push((name, alloc::vec![b])),
            }
        }
        out
    }

    /// Books grouped by series, groups sorted by name, books by series index.
    pub fn series(&self) -> Vec<(String, Vec<&BookEntry>)> {
        let mut out: Vec<(String, Vec<&BookEntry>)> = Vec::new();
        let mut v: Vec<&BookEntry> = self.books.iter().filter(|b| !b.hidden() && b.series.is_some()).collect();
        v.sort_by(|a, b| {
            let (sa, ia) = a.series.as_ref().map(|s| (s.0.to_lowercase(), s.1)).unwrap_or_default();
            let (sb, ib) = b.series.as_ref().map(|s| (s.0.to_lowercase(), s.1)).unwrap_or_default();
            sa.cmp(&sb).then(ia.cmp(&ib)).then_with(|| sort_title(&a.title).cmp(&sort_title(&b.title)))
        });
        for b in v {
            let name = b.series.as_ref().map(|s| s.0.clone()).unwrap_or_default();
            match out.last_mut() {
                Some((n, list)) if *n == name => list.push(b),
                _ => out.push((name, alloc::vec![b])),
            }
        }
        out
    }

    /// Books finished in a day range (inclusive).
    pub fn finished_between(&self, from: u16, to: u16) -> Vec<&BookEntry> {
        let mut v: Vec<&BookEntry> =
            self.books.iter().filter(|b| b.stats.finished.map(|d| d >= from && d <= to).unwrap_or(false)).collect();
        v.sort_by_key(|b| b.stats.finished);
        v
    }

    /// Record the total pages of a book for a typography profile.
    pub fn set_pages_total(&mut self, id: BookId, key: u32, pages: u32) {
        if let Some(b) = self.books.iter_mut().find(|b| b.id == id) {
            if b.pages_total != Some((key, pages)) {
                b.pages_total = Some((key, pages));
                self.dirty = true;
                self.generation = self.generation.wrapping_add(1);
            }
        }
    }

    /// Record that a book was opened now.
    pub fn opened(&mut self, id: BookId, now: u32) {
        self.dirty_pos = true;
        self.generation = self.generation.wrapping_add(1);
        if self.current != Some(id) {
            self.dirty = true;
            self.generation = self.generation.wrapping_add(1);
        }
        self.current = Some(id);
        if let Some(b) = self.books.iter_mut().find(|b| b.id == id) {
            b.last_opened = now;
            if b.status == Status::Unread {
                b.status = Status::Reading;
            }
        }
    }

    /// Update the reading position (Unread becomes Reading; finishing is explicit, see
    /// [`Library::reached_end`]).
    pub fn set_loc(&mut self, id: BookId, loc: Loc) {
        if let Some(b) = self.books.iter_mut().find(|b| b.id == id) {
            if b.loc != loc || b.status == Status::Unread {
                b.loc = loc;
                if b.status == Status::Unread {
                    b.status = Status::Reading;
                }
                self.dirty_pos = true;
                self.generation = self.generation.wrapping_add(1);
            }
        }
    }

    /// The reader showed the last page: mark finished today unless already finished.
    pub fn reached_end(&mut self, id: BookId, today: u16) -> bool {
        if let Some(b) = self.books.iter_mut().find(|b| b.id == id) {
            if b.status != Status::Finished {
                b.status = Status::Finished;
                b.stats.finished = Some(today);
                if b.stats.started.is_none() {
                    b.stats.started = Some(today);
                }
                self.dirty_pos = true;
                self.generation = self.generation.wrapping_add(1);
                return true;
            }
        }
        false
    }

    /// Mark finished (or not) today.
    pub fn set_finished(&mut self, id: BookId, finished: bool, today: u16) {
        self.dirty_pos = true;
        self.generation = self.generation.wrapping_add(1);
        if let Some(b) = self.books.iter_mut().find(|b| b.id == id) {
            if finished {
                b.status = Status::Finished;
                b.stats.finished = Some(today);
            } else {
                b.status = Status::Reading;
                b.stats.finished = None;
            }
        }
    }

    /// Search titles, authors and series (case-insensitive substring; multi-word AND).
    pub fn search(&self, query: &str) -> Vec<&BookEntry> {
        let q: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
        if q.is_empty() {
            return Vec::new();
        }
        self.books
            .iter()
            .filter(|b| !b.hidden())
            .filter(|b| {
                let hay = alloc::format!("{} {} {}", b.title, b.authors.join(" "), b.series.as_ref().map(|s| s.0.as_str()).unwrap_or(""))
                    .to_lowercase();
                q.iter().all(|w| hay.contains(w.as_str()))
            })
            .collect()
    }

    /// The next book in the same series, if any.
    pub fn next_in_series(&self, id: BookId) -> Option<&BookEntry> {
        let b = self.get(id)?;
        let (name, idx) = b.series.as_ref()?;
        self.books
            .iter()
            .filter(|o| !o.missing && o.id != id)
            .filter(|o| o.series.as_ref().map(|s| &s.0 == name && s.1 > *idx).unwrap_or(false))
            .min_by_key(|o| o.series.as_ref().map(|s| s.1).unwrap_or(u16::MAX))
    }
}

/// Title without a leading article, lowercase, for sorting.
pub fn sort_title(t: &str) -> String {
    let l = t.trim().to_lowercase();
    for art in ["the ", "a ", "an "] {
        if let Some(rest) = l.strip_prefix(art) {
            return rest.into();
        }
    }
    l
}

#[cfg(test)]
mod tests {
    use super::*;
    use quire_fs::host::HostFs;

    fn entry(id: u64, title: &str, author: &str) -> BookEntry {
        BookEntry {
            id: BookId(id),
            path: alloc::format!("/Books/{title}.epub"),
            size: 1,
            format: quire_doc::Format::Epub,
            title: title.into(),
            authors: alloc::vec![author.into()],
            series: None,
            year: None,
            language: "en".into(),
            sections: 0,
            chars: 1000,
            has_cover: false,
            ingest: IngestState::Pending,
            error: None,
            added: id as u32,
            last_opened: 0,
            loc: Loc::default(),
            status: Status::Unread,
            collections: Vec::new(),
            stats: BookStats::default(),
            missing: false,
            pages_total: None,
        }
    }

    #[test]
    fn index_round_trips_and_sorts() {
        let dir = std::env::temp_dir().join(alloc::format!("quire-lib-{}", std::process::id()));
        let fs = HostFs::new(&dir);
        let mut lib = Library::load(&fs);
        lib.upsert(entry(1, "The Zebra", "Adams"));
        lib.upsert(entry(2, "Apples", "Zola"));
        lib.upsert(entry(3, "A Middle", "Eliot"));
        let c = lib.add_collection("Victorian");
        lib.toggle_collection(BookId(3), c);
        lib.opened(BookId(2), 100);
        lib.set_loc(BookId(2), Loc { section: 1, pos: Pos::START, chars: 250 });
        lib.save(&fs).unwrap();
        let lib2 = Library::load(&fs);
        assert_eq!(lib2.books.len(), 3);
        assert_eq!(lib2.current, Some(BookId(2)));
        assert_eq!(lib2.get(BookId(2)).unwrap().percent(), 25);
        assert_eq!(lib2.by_title().iter().map(|b| b.title.as_str()).collect::<Vec<_>>(), ["Apples", "A Middle", "The Zebra"]);
        assert_eq!(lib2.by_author().iter().map(|b| b.title.as_str()).collect::<Vec<_>>(), ["The Zebra", "A Middle", "Apples"]);
        assert_eq!(lib2.shelf()[0].id, BookId(2));
        assert_eq!(lib2.in_collection(c).len(), 1);
        assert_eq!(lib2.search("mid eli").len(), 1);
        assert_eq!(lib2.pending().len(), 3);
        // A position change alone rewrites only the positions file.
        let mut lib3 = Library::load(&fs);
        let before = fs.open(INDEX_FILE).map(|f| quire_fs::ReadAt::len(&f)).unwrap();
        lib3.set_loc(BookId(2), Loc { section: 2, pos: Pos::START, chars: 500 });
        assert!(!lib3.dirty && lib3.dirty_pos);
        lib3.save(&fs).unwrap();
        assert_eq!(fs.open(INDEX_FILE).map(|f| quire_fs::ReadAt::len(&f)).unwrap(), before);
        assert_eq!(Library::load(&fs).get(BookId(2)).unwrap().loc.chars, 500);
        assert!(lib3.reached_end(BookId(2), 20_000));
        assert_eq!(lib3.get(BookId(2)).unwrap().status, Status::Finished);
        assert_eq!(lib3.finished_between(19_000, 21_000).len(), 1);
        assert_eq!(lib3.authors().len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
