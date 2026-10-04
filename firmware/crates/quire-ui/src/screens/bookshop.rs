//! 35–39 the Bookshop (offline catalog, shelves, book page, search, browse, downloads),
//! 32 OPDS catalogs, 33 Calibre connect, 34 sync.
//!
//! The catalog never lives in RAM: it is a fixed-record file on the card
//! (`/.quire/bookshop/catalog.qcat`, see [`CatalogFile`]) that the screens read with
//! `read_at` — one 160-byte record per visible row, the shelf and browse index lists
//! precomputed when the file is written. Screens keep only the rows of the page they
//! show and re-read when the page changes, never per draw. Until a catalog has been
//! fetched a small built-in seed is served from memory in the same format.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use quire_fs::{Fs, FsResult, ReadAt};
use quire_gfx::{draw_text, Frame, Ink, Rect, TextStyle};
use serde::{Deserialize, Serialize};

use crate::keyboard::KeyboardScreen;
use crate::net::{DownloadState, FetchRequest, NetEvent, OpdsEntry};
use crate::text::{draw_label, ellipsis, line_h, page_indicator, wrap};
use crate::theme::*;
use crate::widgets::{self, empty_state, poster_tiles, rail, row, running_head, setting_row, stepped_bar, ListNav, RowState, SettingValue};
use crate::{Action, Ctx, Env, Event, Key, KeyEvent, KeyKind, Refresh, Result_, Screen, SysRequest, WifiState};

/// Three picks shown on the empty home: (title, author, hours).
pub const START_HERE: [(&str, &str, &str); 3] = [
    ("Pride and Prejudice", "Jane Austen", "6 h"),
    ("The Adventures of Sherlock Holmes", "Arthur Conan Doyle", "5 h"),
    ("Walden", "Henry David Thoreau", "8 h"),
];

/// Where the offline catalog lives on the card (fixed records, see [`CatalogFile`]).
pub const CATALOG_FILE: &str = "/.quire/bookshop/catalog.qcat";
/// Saved-for-later list.
pub const SAVED_FILE: &str = "/.quire/bookshop/saved.bin";

/// Vertical position of empty states (brief §5.13: one position everywhere).
const EMPTY_Y: i32 = widgets::CONTENT_TOP + 148;

/// One catalog book, fully decoded (only ever one at a time: the book page).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopBook {
    /// Stable id ("pg1342", "se/…").
    pub id: String,
    /// Title.
    pub title: String,
    /// Author.
    pub author: String,
    /// Year.
    pub year: u16,
    /// Language code.
    pub lang: String,
    /// Subjects.
    pub subjects: Vec<String>,
    /// Source name ("Project Gutenberg", "Standard Ebooks", "Creative Commons").
    pub source: String,
    /// Licence line.
    pub licence: String,
    /// Download URL (EPUB).
    pub url: String,
    /// Size in bytes.
    pub size: u32,
    /// Popularity rank (lower is more popular).
    pub rank: u32,
    /// Estimated reading hours × 10.
    pub hours10: u16,
    /// Blurb.
    pub blurb: String,
    /// Collection name, if part of a curated collection.
    pub collection: Option<String>,
    /// Modern (post-1928) or Creative Commons.
    pub modern: bool,
    /// Days since epoch the edition was added.
    pub added: u16,
}

/// A decoded catalog: what the fetcher builds before [`write_catalog`] lays it out on
/// the card. The screens never hold one.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Catalog {
    /// Books.
    pub books: Vec<ShopBook>,
    /// When it was fetched.
    pub fetched: u32,
}

/// A built-in seed so the shop works before any catalog download.
pub fn seed() -> Catalog {
    let mk =
        |id: &str, title: &str, author: &str, year: u16, subj: &str, url: &str, size: u32, rank: u32, hours10: u16, blurb: &str| ShopBook {
            id: id.into(),
            title: title.into(),
            author: author.into(),
            year,
            lang: "en".into(),
            subjects: subj.split(',').map(|s| String::from(s.trim())).collect(),
            source: "Project Gutenberg".into(),
            licence: "Public domain".into(),
            url: url.into(),
            size,
            rank,
            hours10,
            blurb: blurb.into(),
            collection: Some("Start here".into()),
            modern: false,
            added: 0,
        };
    Catalog {
        fetched: 0,
        books: alloc::vec![
            mk("pg1342", "Pride and Prejudice", "Jane Austen", 1813, "romance, classics", "https://www.gutenberg.org/ebooks/1342.epub3.images", 720_000, 1, 60, "Elizabeth Bennet and Mr Darcy misjudge each other, and slowly learn better, in the best-loved comedy of manners in English."),
            mk("pg1661", "The Adventures of Sherlock Holmes", "Arthur Conan Doyle", 1892, "mystery, short stories", "https://www.gutenberg.org/ebooks/1661.epub3.images", 640_000, 2, 50, "Twelve cases from Baker Street, from A Scandal in Bohemia to The Copper Beeches."),
            mk("pg205", "Walden", "Henry David Thoreau", 1854, "nature, philosophy", "https://www.gutenberg.org/ebooks/205.epub3.images", 520_000, 3, 80, "Two years in a cabin by a pond, and what a deliberate life costs and gives."),
            mk("pg2701", "Moby-Dick", "Herman Melville", 1851, "adventure, classics", "https://www.gutenberg.org/ebooks/2701.epub3.images", 1_400_000, 4, 180, "Call me Ishmael. A whaling voyage, a wounded captain, and the white whale."),
            mk("pg84", "Frankenstein", "Mary Shelley", 1818, "gothic, science fiction", "https://www.gutenberg.org/ebooks/84.epub3.images", 480_000, 5, 55, "A young scientist creates a living being and abandons it."),
            mk("pg1232", "The Prince", "Niccolò Machiavelli", 1532, "politics, philosophy", "https://www.gutenberg.org/ebooks/1232.epub3.images", 260_000, 6, 30, "How princes gain and keep power, told without illusions."),
            mk("pg11", "Alice's Adventures in Wonderland", "Lewis Carroll", 1865, "fantasy, children", "https://www.gutenberg.org/ebooks/11.epub3.images", 340_000, 7, 25, "Down the rabbit hole with the Cheshire Cat, the Hatter and the Queen of Hearts."),
            mk("pg2600", "War and Peace", "Leo Tolstoy", 1869, "historical, classics", "https://www.gutenberg.org/ebooks/2600.epub3.images", 3_300_000, 8, 350, "Five families and Napoleon's invasion of Russia."),
            mk("pg145", "Middlemarch", "George Eliot", 1871, "classics", "https://www.gutenberg.org/ebooks/145.epub3.images", 1_500_000, 9, 210, "A study of provincial life: Dorothea, Lydgate, and the town that shapes them."),
            mk("pg1400", "Great Expectations", "Charles Dickens", 1861, "classics", "https://www.gutenberg.org/ebooks/1400.epub3.images", 1_000_000, 10, 130, "Pip, Estella, Magwitch and the fortune that was not what it seemed."),
            mk("pg2814", "Dubliners", "James Joyce", 1914, "short stories", "https://www.gutenberg.org/ebooks/2814.epub3.images", 380_000, 11, 45, "Fifteen stories of a city and its paralysis, ending with The Dead."),
            mk("pg174", "The Picture of Dorian Gray", "Oscar Wilde", 1890, "gothic, classics", "https://www.gutenberg.org/ebooks/174.epub3.images", 520_000, 12, 50, "A portrait ages so that its subject need not."),
            mk("pg35", "The Time Machine", "H. G. Wells", 1895, "science fiction", "https://www.gutenberg.org/ebooks/35.epub3.images", 220_000, 13, 20, "A traveller into the year 802,701 and beyond."),
            mk("pg2680", "Meditations", "Marcus Aurelius", 180, "philosophy", "https://www.gutenberg.org/ebooks/2680.epub3.images", 360_000, 14, 35, "The private notebook of a Roman emperor on how to live."),
            mk("pg43", "The Strange Case of Dr Jekyll and Mr Hyde", "Robert Louis Stevenson", 1886, "gothic, mystery", "https://www.gutenberg.org/ebooks/43.epub3.images", 200_000, 15, 15, "A lawyer investigates his friend's sinister associate."),
            mk("pg1184", "The Count of Monte Cristo", "Alexandre Dumas", 1846, "adventure, revenge", "https://www.gutenberg.org/ebooks/1184.epub3.images", 2_800_000, 16, 300, "Wrongly imprisoned, Edmond Dantès escapes with a fortune and a plan."),
            mk("pg98", "A Tale of Two Cities", "Charles Dickens", 1859, "historical, classics", "https://www.gutenberg.org/ebooks/98.epub3.images", 800_000, 17, 100, "London and Paris in the years of the Revolution."),
            mk("pg1952", "The Yellow Wallpaper", "Charlotte Perkins Gilman", 1892, "short stories", "https://www.gutenberg.org/ebooks/1952.epub3.images", 90_000, 18, 5, "A rest cure, a room, and the pattern on the wall."),
            mk("pg244", "A Study in Scarlet", "Arthur Conan Doyle", 1887, "mystery", "https://www.gutenberg.org/ebooks/244.epub3.images", 300_000, 19, 25, "Holmes and Watson meet, and a body is found in Lauriston Gardens."),
            mk("pg2148", "The Works of Edgar Allan Poe, Volume 2", "Edgar Allan Poe", 1845, "horror, short stories", "https://www.gutenberg.org/ebooks/2148.epub3.images", 700_000, 20, 60, "The Raven, The Tell-Tale Heart, and the rest of the second volume."),
        ],
    }
}

// ---------------------------------------------------------------------------------------
// The catalog file
//
// Little-endian throughout.
//
//   header (64 B): "QCAT", version u16, pad u16, count u32, fetched u32, shelf table
//     offset u32, group directory offset u32, strings offset u32, shelf counts [u8; 6],
//     pad [u8; 2], group counts [u16; 4], reserved.
//   records (160 B each, sorted by rank): id [16], title [56], author [32],
//     subjects [32], year u16, lang [2], flags u8, source u8, hours10 u16, rank u32,
//     size u32, added u16, extra offset u32, extra length u16.
//   shelf table: 6 shelves × 12 record indexes (u32).
//   group directory: for each of the 4 browse kinds, `group counts[k]` entries of
//     name offset u32, name length u16, member count u16, members offset u32; then the
//     member lists (u32 record indexes, in rank order).
//   strings: per book an "extra" blob of eight u16-length-prefixed strings (full title,
//     full author, url, subjects, source, licence, collection, blurb); group names.

const MAGIC: &[u8; 4] = b"QCAT";
const HEADER: usize = 64;
const REC: usize = 160;
const SHELVES: usize = 6;
/// The last focusable column on a shelf: three covers, then the tile that opens the
/// rest of it.
const SHELF_LAST: usize = 3;
const SHELF_LEN: usize = 12;
/// Books a search returns at most.
const SEARCH_LIMIT: usize = 200;
/// Members a browse group lists at most.
const MEMBERS_LIMIT: usize = 1000;
/// Records read per step while scanning the whole catalog (one 2.5 KB buffer).
const SCAN_BATCH: usize = 16;

const SHELF_NAMES: [&str; SHELVES] =
    ["Start here", "Popular this week", "New editions", "Modern & Creative Commons", "Collections", "By subject"];
const KIND_NAMES: [&str; 4] = ["Subjects", "Authors", "Collections", "Languages"];

fn le16(d: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([d[at], d[at + 1]])
}
fn le32(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}
fn put16(d: &mut [u8], at: usize, v: u16) {
    d[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(d: &mut [u8], at: usize, v: u32) {
    d[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
/// Copy `s` into a NUL-padded field of `n` bytes, cut at a character boundary.
fn put_field(d: &mut [u8], at: usize, n: usize, s: &str) {
    let mut end = s.len().min(n);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    d[at..at + end].copy_from_slice(&s.as_bytes()[..end]);
}
fn field(d: &[u8], at: usize, n: usize) -> String {
    let raw = &d[at..at + n];
    let end = raw.iter().position(|b| *b == 0).unwrap_or(n);
    String::from_utf8_lossy(&raw[..end]).into_owned()
}
fn source_code(source: &str) -> u8 {
    if source.contains("Gutenberg") {
        0
    } else if source.contains("Standard") {
        1
    } else {
        2
    }
}
fn push_str(out: &mut Vec<u8>, s: &str) {
    let n = s.len().min(u16::MAX as usize);
    out.extend_from_slice(&(n as u16).to_le_bytes());
    out.extend_from_slice(&s.as_bytes()[..n]);
}

/// Lay a decoded catalog out in the fixed-record format.
pub fn encode(cat: &Catalog) -> Vec<u8> {
    // Records in rank order.
    let mut order: Vec<usize> = (0..cat.books.len()).collect();
    order.sort_by_key(|i| cat.books[*i].rank);
    let books: Vec<&ShopBook> = order.iter().map(|i| &cat.books[*i]).collect();
    let n = books.len();
    let lang_ok = |b: &ShopBook| b.lang.is_empty() || b.lang == "en";
    // Shelves.
    let mut shelves: [Vec<u32>; SHELVES] = Default::default();
    shelves[0] = (0..n).filter(|i| lang_ok(books[*i]) && books[*i].collection.as_deref() == Some("Start here")).map(|i| i as u32).collect();
    shelves[1] = (0..n).filter(|i| lang_ok(books[*i])).map(|i| i as u32).collect();
    let mut newest: Vec<u32> = shelves[1].clone();
    newest.sort_by(|a, b| books[*b as usize].added.cmp(&books[*a as usize].added));
    shelves[2] = newest;
    shelves[3] = (0..n).filter(|i| lang_ok(books[*i]) && books[*i].modern).map(|i| i as u32).collect();
    shelves[4] = (0..n).filter(|i| lang_ok(books[*i]) && books[*i].collection.is_some()).map(|i| i as u32).collect();
    shelves[5] = shelves[1].clone();
    for s in shelves.iter_mut() {
        s.truncate(SHELF_LEN);
    }
    // Browse groups: (name, members) per kind, names sorted case-insensitively.
    let mut groups: [Vec<(String, Vec<u32>)>; 4] = Default::default();
    for (i, b) in books.iter().enumerate() {
        let mut add = |kind: usize, name: &str| {
            if name.is_empty() {
                return;
            }
            let g = &mut groups[kind];
            match g.iter_mut().find(|(n, _)| n == name) {
                Some((_, m)) => m.push(i as u32),
                None => g.push((String::from(name), alloc::vec![i as u32])),
            }
        };
        for s in &b.subjects {
            add(0, s);
        }
        add(1, &b.author);
        if let Some(c) = &b.collection {
            add(2, c);
        }
        add(3, &b.lang.to_uppercase());
    }
    for g in groups.iter_mut() {
        g.sort_by_key(|(name, _)| name.to_lowercase());
    }
    // Layout.
    let recs_off = HEADER;
    let shelf_off = recs_off + n * REC;
    let dir_off = shelf_off + SHELVES * SHELF_LEN * 4;
    let dir_len: usize = groups.iter().map(|g| g.len() * 12).sum();
    let members_off = dir_off + dir_len;
    let members_len: usize = groups.iter().map(|g| g.iter().map(|(_, m)| m.len() * 4).sum::<usize>()).sum();
    let strings_off = members_off + members_len;
    let mut out = alloc::vec![0u8; strings_off];
    let mut strings: Vec<u8> = Vec::new();
    out[0..4].copy_from_slice(MAGIC);
    put16(&mut out, 4, 1);
    put32(&mut out, 8, n as u32);
    put32(&mut out, 12, cat.fetched);
    put32(&mut out, 16, shelf_off as u32);
    put32(&mut out, 20, dir_off as u32);
    put32(&mut out, 24, strings_off as u32);
    for (k, s) in shelves.iter().enumerate() {
        out[28 + k] = s.len() as u8;
    }
    for (k, g) in groups.iter().enumerate() {
        put16(&mut out, 36 + 2 * k, g.len().min(u16::MAX as usize) as u16);
    }
    for (i, b) in books.iter().enumerate() {
        let r = &mut out[recs_off + i * REC..recs_off + (i + 1) * REC];
        put_field(r, 0, 16, &b.id);
        put_field(r, 16, 56, &b.title);
        put_field(r, 72, 32, &b.author);
        put_field(r, 104, 32, &b.subjects.join(", "));
        put16(r, 136, b.year);
        put_field(r, 138, 2, &b.lang);
        r[140] = (b.modern as u8) | ((b.collection.is_some() as u8) << 1);
        r[141] = source_code(&b.source);
        put16(r, 142, b.hours10);
        put32(r, 144, b.rank);
        put32(r, 148, b.size);
        put16(r, 152, b.added);
        let extra_at = strings.len();
        for s in [
            b.title.as_str(),
            b.author.as_str(),
            b.url.as_str(),
            &b.subjects.join(", "),
            b.source.as_str(),
            b.licence.as_str(),
            b.collection.as_deref().unwrap_or(""),
            b.blurb.as_str(),
        ] {
            push_str(&mut strings, s);
        }
        put32(r, 154, (strings_off + extra_at) as u32);
        put16(r, 158, (strings.len() - extra_at).min(u16::MAX as usize) as u16);
    }
    for (k, s) in shelves.iter().enumerate() {
        for (j, idx) in s.iter().enumerate() {
            put32(&mut out, shelf_off + (k * SHELF_LEN + j) * 4, *idx);
        }
    }
    let mut dir_at = dir_off;
    let mut mem_at = members_off;
    for g in groups.iter() {
        for (name, members) in g {
            let name_at = strings.len();
            strings.extend_from_slice(name.as_bytes());
            put32(&mut out, dir_at, (strings_off + name_at) as u32);
            put16(&mut out, dir_at + 4, name.len().min(u16::MAX as usize) as u16);
            put16(&mut out, dir_at + 6, members.len().min(u16::MAX as usize) as u16);
            put32(&mut out, dir_at + 8, mem_at as u32);
            dir_at += 12;
            for m in members {
                put32(&mut out, mem_at, *m);
                mem_at += 4;
            }
        }
    }
    out.extend_from_slice(&strings);
    out
}

/// Write a fetched catalog to the card in the fixed-record format.
pub fn write_catalog<F: Fs>(fs: &F, cat: &Catalog) -> FsResult<()> {
    let bytes = encode(cat);
    fs.mkdir_all("/.quire/bookshop")?;
    fs.write_atomic(CATALOG_FILE, &bytes)
}

/// A catalog source: the file on the card, or the seed in memory.
pub enum Source<F: ReadAt> {
    /// The fetched catalog file.
    File(F),
    /// The built-in seed, encoded.
    Mem(Vec<u8>),
}

impl<F: ReadAt> ReadAt for Source<F> {
    fn len(&self) -> u64 {
        match self {
            Source::File(f) => f.len(),
            Source::Mem(v) => v.len() as u64,
        }
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        match self {
            Source::File(f) => f.read_at(offset, buf),
            Source::Mem(v) => v.read_at(offset, buf),
        }
    }
}

/// The fields of a record a list row needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    /// Record index.
    pub index: u32,
    /// Stable id.
    pub id: String,
    /// Title (may be shortened to the record field).
    pub title: String,
    /// Author (may be shortened).
    pub author: String,
    /// Reading hours × 10.
    pub hours10: u16,
    /// Source code: 0 Gutenberg, 1 Standard Ebooks, 2 Creative Commons.
    pub source: u8,
}

/// A browse group: name, member count and where its member list lives.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Group {
    /// Name.
    pub name: String,
    /// Members.
    pub count: u16,
    members_off: u32,
}

/// A catalog open for reading.
pub struct CatalogFile<R: ReadAt> {
    src: R,
    count: u32,
    /// When it was fetched (0 for the seed).
    pub fetched: u32,
    shelf_off: u32,
    dir_off: u32,
    shelf_n: [u8; SHELVES],
    group_n: [u16; 4],
}

/// Open the catalog on the card. Without one the seed is laid out in the same format
/// and written to the card once (so later opens are a header read like any catalog);
/// on a card that cannot be written it is served from memory.
pub fn open_catalog<F: Fs>(fs: &F) -> CatalogFile<Source<F::File>> {
    if let Ok(file) = fs.open(CATALOG_FILE) {
        if let Some(c) = CatalogFile::parse(Source::File(file)) {
            if c.count > 0 {
                return c;
            }
        }
    }
    let bytes = encode(&seed());
    if fs.mkdir_all("/.quire/bookshop").is_ok() && fs.write_atomic(CATALOG_FILE, &bytes).is_ok() {
        if let Ok(file) = fs.open(CATALOG_FILE) {
            if let Some(c) = CatalogFile::parse(Source::File(file)) {
                return c;
            }
        }
    }
    CatalogFile::parse(Source::Mem(bytes)).unwrap_or_else(|| CatalogFile {
        src: Source::Mem(Vec::new()),
        count: 0,
        fetched: 0,
        shelf_off: 0,
        dir_off: 0,
        shelf_n: [0; SHELVES],
        group_n: [0; 4],
    })
}

impl<R: ReadAt> CatalogFile<R> {
    /// Read the header of an encoded catalog.
    pub fn parse(src: R) -> Option<Self> {
        let mut h = [0u8; HEADER];
        src.read_exact_at(0, &mut h).ok()?;
        if &h[0..4] != MAGIC || le16(&h, 4) != 1 {
            return None;
        }
        let count = le32(&h, 8);
        let shelf_off = le32(&h, 16);
        let dir_off = le32(&h, 20);
        if (HEADER as u64 + count as u64 * REC as u64) > src.len() {
            return None;
        }
        let mut shelf_n = [0u8; SHELVES];
        shelf_n.copy_from_slice(&h[28..28 + SHELVES]);
        let mut group_n = [0u16; 4];
        for (k, g) in group_n.iter_mut().enumerate() {
            *g = le16(&h, 36 + 2 * k);
        }
        Some(CatalogFile { src, count, fetched: le32(&h, 12), shelf_off, dir_off, shelf_n, group_n })
    }
    /// Books in the catalog.
    pub fn count(&self) -> u32 {
        self.count
    }
    fn record(&self, i: u32, buf: &mut [u8; REC]) -> bool {
        i < self.count && self.src.read_exact_at(HEADER as u64 + i as u64 * REC as u64, buf).is_ok()
    }
    fn row_from(i: u32, r: &[u8]) -> Row {
        Row { index: i, id: field(r, 0, 16), title: field(r, 16, 56), author: field(r, 72, 32), hours10: le16(r, 142), source: r[141] }
    }
    /// The list-row fields of record `i`.
    pub fn row(&self, i: u32) -> Option<Row> {
        let mut r = [0u8; REC];
        if !self.record(i, &mut r) {
            return None;
        }
        Some(Self::row_from(i, &r))
    }
    /// Rows for a set of record indexes (missing ones are skipped).
    pub fn rows(&self, ids: &[u32]) -> Vec<Row> {
        ids.iter().filter_map(|i| self.row(*i)).collect()
    }
    /// The whole book behind record `i` (reads its strings blob).
    pub fn book(&self, i: u32) -> Option<ShopBook> {
        let mut r = [0u8; REC];
        if !self.record(i, &mut r) {
            return None;
        }
        let extra_off = le32(&r, 154) as u64;
        let extra_len = le16(&r, 158) as usize;
        let extra = self.src.read_range(extra_off, extra_len).ok()?;
        let mut at = 0;
        let mut next = || -> String {
            if at + 2 > extra.len() {
                return String::new();
            }
            let n = le16(&extra, at) as usize;
            at += 2;
            let end = (at + n).min(extra.len());
            let s = String::from_utf8_lossy(&extra[at..end]).into_owned();
            at = end;
            s
        };
        let title = next();
        let author = next();
        let url = next();
        let subjects = next();
        let source = next();
        let licence = next();
        let collection = next();
        let blurb = next();
        Some(ShopBook {
            id: field(&r, 0, 16),
            title: if title.is_empty() { field(&r, 16, 56) } else { title },
            author: if author.is_empty() { field(&r, 72, 32) } else { author },
            year: le16(&r, 136),
            lang: field(&r, 138, 2),
            subjects: subjects.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).map(String::from).collect(),
            source,
            licence,
            url,
            size: le32(&r, 148),
            rank: le32(&r, 144),
            hours10: le16(&r, 142),
            blurb,
            collection: if collection.is_empty() { None } else { Some(collection) },
            modern: r[140] & 1 != 0,
            added: le16(&r, 152),
        })
    }
    /// Record indexes on shelf `k` (see `SHELF_NAMES`).
    pub fn shelf(&self, k: usize) -> Vec<u32> {
        if k >= SHELVES {
            return Vec::new();
        }
        let n = self.shelf_n[k] as usize;
        let mut buf = [0u8; SHELF_LEN * 4];
        if n == 0 || self.src.read_exact_at(self.shelf_off as u64 + (k * SHELF_LEN * 4) as u64, &mut buf[..n * 4]).is_err() {
            return Vec::new();
        }
        (0..n).map(|j| le32(&buf, j * 4)).collect()
    }
    /// The browse groups of a kind (0 subjects, 1 authors, 2 collections, 3 languages).
    pub fn groups(&self, kind: usize) -> Vec<Group> {
        if kind >= 4 {
            return Vec::new();
        }
        let before: u32 = self.group_n[..kind].iter().map(|n| *n as u32).sum();
        let n = self.group_n[kind] as usize;
        let mut dir = alloc::vec![0u8; n * 12];
        if self.src.read_exact_at(self.dir_off as u64 + before as u64 * 12, &mut dir).is_err() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(n);
        for j in 0..n {
            let e = &dir[j * 12..j * 12 + 12];
            let name_off = le32(e, 0) as u64;
            let name_len = le16(e, 4) as usize;
            let name = self.src.read_range(name_off, name_len).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
            out.push(Group { name, count: le16(e, 6), members_off: le32(e, 8) });
        }
        out
    }
    /// Record indexes in a group, in rank order.
    pub fn members(&self, g: &Group) -> Vec<u32> {
        let n = (g.count as usize).min(MEMBERS_LIMIT);
        let mut buf = alloc::vec![0u8; n * 4];
        if self.src.read_exact_at(g.members_off as u64, &mut buf).is_err() {
            return Vec::new();
        }
        (0..n).map(|j| le32(&buf, j * 4)).collect()
    }
    /// Walk every record in batches; `f` gets (index, record) and returns false to stop.
    fn scan(&self, mut f: impl FnMut(u32, &[u8]) -> bool) {
        let mut buf = [0u8; REC * SCAN_BATCH];
        let mut i = 0u32;
        while i < self.count {
            let n = ((self.count - i) as usize).min(SCAN_BATCH);
            if self.src.read_exact_at(HEADER as u64 + i as u64 * REC as u64, &mut buf[..n * REC]).is_err() {
                return;
            }
            for k in 0..n {
                if !f(i + k as u32, &buf[k * REC..(k + 1) * REC]) {
                    return;
                }
            }
            i += n as u32;
        }
    }
    /// Records whose title, author or subjects contain every word of `query`
    /// (ASCII case-insensitive), in rank order, at most `SEARCH_LIMIT`.
    pub fn search(&self, query: &str) -> Vec<u32> {
        let q = query.to_ascii_lowercase();
        let words: Vec<&[u8]> = q.split_whitespace().map(|w| w.as_bytes()).collect();
        if words.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut hay = [0u8; 56 + 1 + 32 + 1 + 32];
        self.scan(|i, r| {
            hay[..56].copy_from_slice(&r[16..72]);
            hay[56] = b' ';
            hay[57..89].copy_from_slice(&r[72..104]);
            hay[89] = b' ';
            hay[90..].copy_from_slice(&r[104..136]);
            for b in hay.iter_mut() {
                *b = if *b == 0 { b' ' } else { b.to_ascii_lowercase() };
            }
            if words.iter().all(|w| hay.windows(<[u8]>::len(w)).any(|h| h == *w)) {
                out.push(i);
            }
            out.len() < SEARCH_LIMIT
        });
        out
    }
    /// Record indexes of the given ids, in the ids' order (one pass over the file).
    pub fn find_ids(&self, ids: &[String]) -> Vec<u32> {
        let mut found: Vec<Option<u32>> = alloc::vec![None; ids.len()];
        let mut left = ids.len();
        self.scan(|i, r| {
            let id = field(r, 0, 16);
            for (k, want) in ids.iter().enumerate() {
                if found[k].is_none() && *want == id {
                    found[k] = Some(i);
                    left -= 1;
                }
            }
            left > 0
        });
        found.into_iter().flatten().collect()
    }
}

/// Saved-for-later ids.
pub fn load_saved<F: Fs>(fs: &F) -> Vec<String> {
    fs.read_to_vec(SAVED_FILE).ok().and_then(|b| postcard::from_bytes(&b).ok()).unwrap_or_default()
}

fn save_saved<F: Fs>(fs: &F, saved: &[String]) {
    if let Ok(b) = postcard::to_allocvec(saved) {
        let _ = fs.mkdir_all("/.quire/bookshop");
        let _ = fs.write_atomic(SAVED_FILE, &b);
    }
}

fn hours_text(h10: u16) -> String {
    if h10 < 10 {
        alloc::format!("{} min", h10 as u32 * 6)
    } else {
        alloc::format!("{} h", h10 / 10)
    }
}

/// "0.7 MB" / "12 MB" / "—".
fn fmt_mb(b: u64) -> String {
    if b == 0 {
        String::from("—")
    } else if b < 10_000_000 {
        alloc::format!("{}.{} MB", b / 1_000_000, (b % 1_000_000) / 100_000)
    } else {
        alloc::format!("{} MB", b / 1_000_000)
    }
}

/// Whether a catalog book is already in the library (by title and author).
fn in_library<E: Env>(cx: &Ctx<E>, title: &str, author: &str) -> Option<quire_library::BookId> {
    cx.lib
        .books
        .iter()
        .find(|x| {
            !x.missing && x.title.eq_ignore_ascii_case(title) && x.authors.first().map(|a| a.eq_ignore_ascii_case(author)).unwrap_or(false)
        })
        .map(|x| x.id)
}

fn src_glyph(source: u8) -> &'static str {
    match source {
        0 => "PG",
        1 => "SE",
        _ => "CC",
    }
}

/// The rows of one list page, re-read from the card only when the page changes.
#[derive(Default)]
struct PageRows {
    page: Option<usize>,
    rows: Vec<Row>,
}

impl PageRows {
    fn invalidate(&mut self) {
        self.page = None;
    }
    /// Rows for the current page of `ids`.
    fn ensure<F: Fs>(&mut self, fs: &F, ids: &[u32], nav: &ListNav) -> &[Row] {
        if self.page != Some(nav.page()) {
            let cat = open_catalog(fs);
            let vis = nav.visible();
            self.rows = cat.rows(&ids[vis.start.min(ids.len())..vis.end.min(ids.len())]);
            self.page = Some(nav.page());
        }
        &self.rows
    }
}

/// Draw a row for a catalog book: "Title" with the author beneath and a value.
fn book_row<E: Env>(cx: &Ctx<E>, f: &mut Frame, y: i32, row_h: i32, r: &Row, focused: bool, glyph: bool) {
    let v = if in_library(cx, &r.title, &r.author).is_some() {
        String::from("in library")
    } else if glyph {
        alloc::format!("{} · {}", src_glyph(r.source), hours_text(r.hours10))
    } else {
        hours_text(r.hours10)
    };
    row(f, y, row_h, &r.title, Some(&r.author), Some(&v), if focused { RowState::Focused } else { RowState::Normal });
}

// ---------------------------------------------------------------------------------------
// 35 home

/// The Bookshop home: six shelves of three covers plus More.
pub struct BookshopHome {
    /// The first three rows of each shelf, read once.
    shelves: Vec<Vec<Row>>,
    loaded: bool,
    focus: (usize, usize),
    first: bool,
}

impl BookshopHome {
    /// New.
    pub fn new() -> Self {
        BookshopHome { shelves: Vec::new(), loaded: false, focus: (0, 0), first: true }
    }
    fn ensure<E: Env>(&mut self, cx: &Ctx<E>) {
        if self.loaded {
            return;
        }
        let cat = open_catalog(cx.env.fs());
        self.first = cat.fetched == 0;
        self.shelves = (0..SHELVES)
            .map(|k| {
                let ids = cat.shelf(k);
                cat.rows(&ids[..ids.len().min(3)])
            })
            .collect();
        self.loaded = true;
    }
}

impl Default for BookshopHome {
    fn default() -> Self {
        Self::new()
    }
}

/// Shop shelf cover cell size.
const SHOP_COVER_W: i32 = 112;
const SHOP_COVER_H: i32 = 152;

/// Draw a small typographic cover cell for a catalog book.
fn shop_cover(f: &mut Frame, x: i32, y: i32, title: &str, author: &str, focused: bool) {
    let r = Rect::new(x, y, SHOP_COVER_W as u32, SHOP_COVER_H as u32);
    widgets::typographic_cover(f, r, title, author);
    f.stroke_rect(r, if focused { 4 } else { 1 }, Ink::Black);
}

impl BookshopHome {
    /// The columns the cursor can rest on along shelf `row` (1-based): each cover the
    /// shelf actually has, then the tile that opens the rest of it. A column past the
    /// last cover draws nothing, so it is never a place.
    fn places(&self, row: usize) -> alloc::vec::Vec<usize> {
        let covers = self.shelves.get(row.wrapping_sub(1)).map(|r| r.len().min(SHELF_LAST)).unwrap_or(0);
        (0..covers).chain(core::iter::once(SHELF_LAST)).collect()
    }

    /// One step in reading order, `dir` = 1 forward or -1 back: along a shelf, then on
    /// to the next, with the search row before the first shelf.
    fn step(&self, dir: i32) -> (usize, usize) {
        let (row, col) = self.focus;
        if dir > 0 {
            if row == 0 {
                return (1, self.places(1)[0]);
            }
            let places = self.places(row);
            let at = places.iter().position(|c| *c == col).unwrap_or(0);
            if at + 1 < places.len() {
                (row, places[at + 1])
            } else if row < SHELVES {
                (row + 1, self.places(row + 1)[0])
            } else {
                (row, col)
            }
        } else {
            if row == 0 {
                return (0, 0);
            }
            let places = self.places(row);
            let at = places.iter().position(|c| *c == col).unwrap_or(0);
            if at > 0 {
                (row, places[at - 1])
            } else if row > 1 {
                (row - 1, *self.places(row - 1).last().unwrap_or(&SHELF_LAST))
            } else {
                (0, 0)
            }
        }
    }
}

impl<E: Env> Screen<E> for BookshopHome {
    fn name(&self) -> &'static str {
        "35-bookshop"
    }
    fn draw(&mut self, cx: &mut Ctx<E>, f: &mut Frame) -> Refresh {
        self.ensure(cx);
        running_head(f, "Bookshop", None);
        let w = f.width() as i32;
        let fl = quire_fonts::ui::label();
        let mut y = widgets::CONTENT_TOP - 4;
        // Search line.
        let sr = Rect::new(widgets::INSET, y, (w - 2 * widgets::INSET) as u32, 36);
        widgets::text_field(f, sr, "", "Search · type on your phone", self.focus.0 == 0);
        y += 44;
        if self.first {
            draw_text(f, fl, widgets::INSET, y + fl.ascent(), "Free, open books. No account, no DRM.", TextStyle::INK);
            y += line_h(fl) + 6;
        }
        let shelf_h = (f.height() as i32 - RAIL_H - y) / 3;
        let per_row = 3;
        let visible_start = ((self.focus.0.saturating_sub(1)) / 3) * 3;
        for (si, rows) in self.shelves.iter().enumerate().skip(visible_start).take(3) {
            let sy = y + (si - visible_start) as i32 * shelf_h;
            draw_label(f, widgets::INSET, sy + fl.ascent(), SHELF_NAMES[si], false);
            let cy = sy + line_h(fl) + 4;
            let cell_w = SHOP_COVER_W;
            // Three covers, a More cell that ends on the margin, equal gaps between.
            let more_w = 64;
            let gap = (w - 2 * widgets::INSET - per_row as i32 * cell_w - more_w) / per_row as i32;
            for (k, r) in rows.iter().take(per_row).enumerate() {
                let x = widgets::INSET + k as i32 * (cell_w + gap);
                let focused = self.focus == (si + 1, k);
                if (shelf_h - line_h(fl) - 8) < SHOP_COVER_H {
                    // Not enough room for a full cover: compact rows.
                    draw_text(
                        f,
                        quire_fonts::ui::label(),
                        x,
                        cy + fl.ascent(),
                        &ellipsis(fl, &r.title, cell_w),
                        TextStyle { inverted: focused, ..TextStyle::INK },
                    );
                } else {
                    shop_cover(f, x, cy, &r.title, &r.author, focused);
                }
            }
            let mx = widgets::INSET + per_row as i32 * (cell_w + gap);
            let more_focused = self.focus == (si + 1, per_row);
            let mr = Rect::new(mx, cy, (w - widgets::INSET - mx).max(24) as u32, SHOP_COVER_H.min(shelf_h - line_h(fl) - 8).max(24) as u32);
            if more_focused {
                f.fill_rect(mr, Ink::Black);
            }
            f.stroke_rect(mr, 1, Ink::Black);
            crate::text::draw_centered(
                f,
                fl,
                mr.x + mr.w as i32 / 2,
                mr.y + mr.h as i32 / 2 + 6,
                "More",
                TextStyle { inverted: more_focused, ..TextStyle::INK },
            );
        }
        rail(f, ["Browse", "Back", "Open", "Search"], None);
        Refresh::Gc
    }
    fn key(&mut self, cx: &mut Ctx<E>, ev: KeyEvent) -> Action<E> {
        if ev.kind != KeyKind::Press {
            return Action::None;
        }
        self.ensure(cx);
        match ev.key {
            Key::Back => Action::Pop,
            Key::Left if self.focus.0 == 0 => Action::Push(Box::new(Browse::new())),
            Key::Right if self.focus.0 == 0 => Action::Push(Box::new(Search::new())),
            // The side keys sit left and right of the screen, and step through the
            // shelves in reading order: along a shelf, then on to the next one. They
            // used to jump a whole shelf at a time, which left moving along a shelf to
            // the bottom keys alone — and on the search row, where focus starts, those
            // open Browse and Search instead, so the keys that plainly move went
            // down and down and never along.
            Key::Up => {
                self.focus = self.step(-1);
                Action::Redraw
            }
            Key::Down => {
                self.focus = self.step(1);
                Action::Redraw
            }
            // The bottom keys move along the shelf the cursor is on, between the places
            // that are there; they used to stop on columns past a shelf's last cover,
            // where nothing is drawn, so the cursor simply disappeared.
            Key::Left | Key::Right if self.focus.0 > 0 => {
                let row = self.focus.0;
                let places = self.places(row);
                let at = places.iter().position(|c| *c == self.focus.1).unwrap_or(0);
                let to = if ev.key == Key::Left { at.saturating_sub(1) } else { (at + 1).min(places.len() - 1) };
                self.focus.1 = places[to];
                Action::Redraw
            }
            // On the search row Left and Right open Browse and Search, above.
            Key::Left | Key::Right => Action::None,
            Key::Confirm => {
                if self.focus.0 == 0 {
                    return Action::Push(Box::new(Search::new()));
                }
                let k = self.focus.0 - 1;
                let rows = &self.shelves[k];
                if self.focus.1 >= 3 || self.focus.1 >= rows.len() {
                    let ids = open_catalog(cx.env.fs()).shelf(k);
                    return Action::Push(Box::new(ShelfList::new(SHELF_NAMES[k], ids)));
                }
                Action::Push(Box::new(BookPage::new(rows[self.focus.1].index)))
            }
            Key::Power => Action::None,
        }
    }
    fn event(&mut self, _cx: &mut Ctx<E>, ev: &Event) -> Action<E> {
        if matches!(ev, Event::Net(NetEvent::Shelves)) {
            self.loaded = false;
            return Action::Redraw;
        }
        Action::None
    }
}

/// A full shelf as a list.
pub struct ShelfList {
    title: String,
    ids: Vec<u32>,
    rows: PageRows,
    nav: ListNav,
}

impl ShelfList {
    /// New, over record indexes.
    pub fn new(title: &str, ids: Vec<u32>) -> Self {
        let n = ids.len();
        ShelfList { title: title.into(), ids, rows: PageRows::default(), nav: ListNav::new(n, 10) }
    }
}

impl<E: Env> Screen<E> for ShelfList {
    fn name(&self) -> &'static str {
        "38-browse-shelf"
    }
    fn draw(&mut self, cx: &mut Ctx<E>, f: &mut Frame) -> Refresh {
        let row_h = ROW_H;
        let per = widgets::rows_between(widgets::CONTENT_TOP, f.height() as i32 - RAIL_H, row_h);
        if per != self.nav.per_page {
            self.nav.per_page = per;
            self.rows.invalidate();
        }
        running_head(f, &self.title, Some(&page_indicator(self.nav.page(), self.nav.pages())));
        let mut y = widgets::CONTENT_TOP;
        let start = self.nav.visible().start;
        let rows = self.rows.ensure(cx.env.fs(), &self.ids, &self.nav);
        for (k, r) in rows.iter().enumerate() {
            book_row(cx, f, y, row_h, r, start + k == self.nav.focus, false);
            y += row_h;
        }
        rail(f, ["", "Back", "Open", ""], None);
        Refresh::Gc
    }
    fn key(&mut self, _cx: &mut Ctx<E>, ev: KeyEvent) -> Action<E> {
        if ev.kind == KeyKind::Release {
            return Action::None;
        }
        if ev.is(Key::Back) {
            return Action::Pop;
        }
        if ev.is(Key::Confirm) {
            return match self.ids.get(self.nav.focus) {
                Some(i) => Action::Push(Box::new(BookPage::new(*i))),
                None => Action::None,
            };
        }
        if self.nav.key(ev) {
            return Action::Redraw;
        }
        Action::None
    }
}

// ---------------------------------------------------------------------------------------
// 36 book page

/// A catalog book's page.
pub struct BookPage {
    index: u32,
    /// The book, read once on the first draw.
    book: Option<ShopBook>,
    /// Whether it is on the saved list (read once).
    saved: Option<bool>,
    /// The blurb wrapped for the page width.
    blurb: Vec<String>,
    page: usize,
    focus: usize,
}

impl BookPage {
    /// New, for a catalog record.
    pub fn new(index: u32) -> Self {
        BookPage { index, book: None, saved: None, blurb: Vec::new(), page: 0, focus: 0 }
    }
    fn ensure<E: Env>(&mut self, cx: &Ctx<E>, width: i32) {
        if self.book.is_none() {
            let fs = cx.env.fs();
            self.book = open_catalog(fs).book(self.index);
            if let Some(b) = &self.book {
                self.blurb = wrap(quire_fonts::ui::body(), &b.blurb, width);
                self.saved = Some(load_saved(fs).contains(&b.id));
            }
        }
    }
    fn download_state<E: Env>(&self, cx: &mut Ctx<E>, url: &str) -> Option<DownloadState> {
        cx.env.net().downloads().iter().find(|d| d.url == url).map(|d| d.state.clone())
    }
}

impl<E: Env> Screen<E> for BookPage {
    fn name(&self) -> &'static str {
        "36-book"
    }
    fn draw(&mut self, cx: &mut Ctx<E>, f: &mut Frame) -> Refresh {
        let w = f.width() as i32;
        self.ensure(cx, w - 2 * widgets::INSET);
        running_head(f, "Bookshop", None);
        let Some(b) = self.book.as_ref() else {
            empty_state(f, EMPTY_Y, "This book is not in the catalog", "Back returns to the shop.");
            rail(f, ["", "Back", "", ""], None);
            return Refresh::Gc;
        };
        let ft = quire_fonts::ui::title();
        let fb = quire_fonts::ui::body();
        let fl = quire_fonts::ui::label();
        let mut y = widgets::CONTENT_TOP;
        shop_cover(f, widgets::INSET, y, &b.title, &b.author, false);
        let tx = widgets::INSET + 96 + 20;
        let tw = w - tx - widgets::INSET;
        let mut ty = y;
        for l in wrap(ft, &b.title, tw).iter().take(3) {
            draw_text(f, ft, tx, ty + ft.ascent(), l, TextStyle::INK);
            ty += line_h(ft);
        }
        draw_text(f, fb, tx, ty + fb.ascent(), &ellipsis(fb, &b.author, tw), TextStyle::INK);
        ty += line_h(fb);
        draw_text(f, fl, tx, ty + fl.ascent() + 4, &alloc::format!("{} · {}", b.year, b.lang.to_uppercase()), TextStyle::INK);
        y += 132 + 16;
        let tiles = alloc::vec![(hours_text(b.hours10), String::from("To read")), (fmt_mb(b.size as u64), String::from("Size"))];
        y = poster_tiles(f, widgets::INSET, y, w - 2 * widgets::INSET, &tiles, 2) + 12;
        draw_text(
            f,
            fl,
            widgets::INSET,
            y + fl.ascent(),
            &ellipsis(fl, &alloc::format!("{} · {}", b.source, b.licence), w - 2 * widgets::INSET),
            TextStyle::INK,
        );
        y += line_h(fl) + 10;
        // Blurb, paginated over the wrapped lines.
        let per = ((f.height() as i32 - RAIL_H - y - ROW_H - 20) / line_h(fb)).max(2) as usize;
        let pages = self.blurb.len().div_ceil(per).max(1);
        let page = self.page.min(pages - 1);
        for l in self.blurb.iter().skip(page * per).take(per) {
            draw_text(f, fb, widgets::INSET, y + fb.ascent(), l, TextStyle::INK);
            y += line_h(fb);
        }
        // Primary state line.
        let state = self.download_state(cx, &b.url);
        let in_lib = in_library(cx, &b.title, &b.author);
        let wifi_on = matches!(cx.env.wifi(), WifiState::Connected { .. });
        let sy = f.height() as i32 - RAIL_H - ROW_H - 8;
        let (label, action) = match (&state, in_lib, wifi_on) {
            (_, Some(_), _) => ("In your library", "Read"),
            (Some(DownloadState::Working), _, _) | (Some(DownloadState::Queued), _, _) => ("Downloading", ""),
            (Some(DownloadState::Retrying(_)), _, _) => ("Gutenberg is limiting requests, retrying", ""),
            (Some(DownloadState::Failed(_)), _, _) => ("Couldn't download", "Retry"),
            (_, None, false) => ("Wi-Fi off · Connect to get this book", "Wi-Fi"),
            _ => ("Get", "Get"),
        };
        if let Some(DownloadState::Working) = &state {
            let d = cx.env.net().downloads().iter().find(|d| d.url == b.url).cloned();
            if let Some(d) = d {
                stepped_bar(
                    f,
                    Rect::new(widgets::INSET, sy + 20, (w - 2 * widgets::INSET) as u32, 16),
                    d.total.map(|t| (d.done * 1000 / t.max(1)) as u32).unwrap_or(0),
                );
            }
        } else if in_lib.is_none() && !wifi_on {
            // A status line above the rail, not an inverted band.
            draw_text(f, fl, widgets::INSET, sy + 20 + fl.ascent(), label, TextStyle::INK);
        } else {
            setting_row(
                f,
                sy,
                ROW_H,
                label,
                &SettingValue::Text(String::from(action)),
                if self.focus == 0 { RowState::Focused } else { RowState::Normal },
            );
        }
        let saved = self.saved.unwrap_or(false);
        rail(f, [if saved { "Saved ✓" } else { "Save" }, "Back", action, "More"], None);
        Refresh::Gc
    }
    fn key(&mut self, cx: &mut Ctx<E>, ev: KeyEvent) -> Action<E> {
        if ev.kind != KeyKind::Press {
            return Action::None;
        }
        let Some(b) = self.book.clone() else {
            return if ev.key == Key::Back { Action::Pop } else { Action::None };
        };
        match ev.key {
            Key::Back => Action::Pop,
            Key::Left => {
                let mut saved = load_saved(cx.env.fs());
                if let Some(i) = saved.iter().position(|s| *s == b.id) {
                    saved.remove(i);
                    self.saved = Some(false);
                } else {
                    saved.push(b.id.clone());
                    self.saved = Some(true);
                }
                save_saved(cx.env.fs(), &saved);
                Action::Redraw
            }
            Key::Right => {
                self.page += 1;
                Action::Redraw
            }
            Key::Down => Action::Push(Box::new(Search::with_query(&b.author))),
            Key::Confirm => {
                if let Some(id) = in_library(cx, &b.title, &b.author) {
                    return Action::Open(id);
                }
                if !matches!(cx.env.wifi(), WifiState::Connected { .. }) {
                    return Action::Push(Box::new(super::wifi::WifiScreen::new_with_hint("Connect to get this book")));
                }
                cx.env.request(SysRequest::Fetch(FetchRequest::Book {
                    url: b.url.clone(),
                    title: b.title.clone(),
                    author: b.author.clone(),
                    size: Some(b.size as u64),
                }));
                Action::Redraw
            }
            _ => Action::None,
        }
    }
    fn event(&mut self, _cx: &mut Ctx<E>, ev: &Event) -> Action<E> {
        match ev {
            Event::Net(NetEvent::Downloads) | Event::BooksChanged | Event::Ingest { .. } | Event::Wifi(_) => Action::Redraw,
            _ => Action::None,
        }
    }
}

// ---------------------------------------------------------------------------------------
// 37 search, 38 browse

/// Search the offline catalog as you type.
pub struct Search {
    query: String,
    /// The query `results` were computed for.
    searched: Option<String>,
    results: Vec<u32>,
    rows: PageRows,
    nav: ListNav,
}

impl Search {
    /// New.
    pub fn new() -> Self {
        Search { query: String::new(), searched: None, results: Vec::new(), rows: PageRows::default(), nav: ListNav::new(0, 9) }
    }
    /// With a query.
    pub fn with_query(q: &str) -> Self {
        Search { query: q.into(), ..Self::new() }
    }
    /// Run the search once per query change (one pass over the catalog records).
    fn ensure<E: Env>(&mut self, cx: &Ctx<E>) {
        if self.searched.as_deref() != Some(self.query.as_str()) {
            self.results = if self.query.trim().is_empty() { Vec::new() } else { open_catalog(cx.env.fs()).search(&self.query) };
            self.searched = Some(self.query.clone());
            self.nav.set_n(self.results.len());
            self.rows.invalidate();
        }
    }
}

impl Default for Search {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: Env> Screen<E> for Search {
    fn name(&self) -> &'static str {
        "37-search"
    }
    fn draw(&mut self, cx: &mut Ctx<E>, f: &mut Frame) -> Refresh {
        self.ensure(cx);
        running_head(f, "Search", None);
        let w = f.width() as i32;
        let y = widgets::CONTENT_TOP - 4;
        widgets::text_field(
            f,
            Rect::new(widgets::INSET, y, (w - 2 * widgets::INSET) as u32, 40),
            &self.query,
            "Title, author or subject",
            true,
        );
        let row_h = ROW_H;
        let top = y + 52;
        let per = widgets::rows_between(top, f.height() as i32 - RAIL_H, row_h);
        if per != self.nav.per_page {
            self.nav.per_page = per;
            self.rows.invalidate();
        }
        let start = self.nav.visible().start;
        let rows = self.rows.ensure(cx.env.fs(), &self.results, &self.nav);
        let mut yy = top;
        for (k, r) in rows.iter().enumerate() {
            book_row(cx, f, yy, row_h, r, start + k == self.nav.focus, true);
            yy += row_h;
        }
        if self.results.is_empty() && !self.query.is_empty() {
            empty_state(f, EMPTY_Y, "Nothing found", "Try the author's surname, or Browse by subject.");
        }
        rail(f, ["", "Back", "Open", "Type"], None);
        Refresh::Gc
    }
    fn key(&mut self, cx: &mut Ctx<E>, ev: KeyEvent) -> Action<E> {
        if ev.kind == KeyKind::Release {
            return Action::None;
        }
        if ev.is(Key::Back) {
            return Action::Pop;
        }
        if ev.is(Key::Right) || ev.is_long(Key::Confirm) {
            return Action::Push(KeyboardScreen::t9("Search", &self.query, "Title, author or subject").boxed());
        }
        if ev.is(Key::Confirm) {
            self.ensure(cx);
            return match self.results.get(self.nav.focus) {
                Some(i) => Action::Push(Box::new(BookPage::new(*i))),
                None => Action::None,
            };
        }
        if self.nav.key(ev) {
            return Action::Redraw;
        }
        Action::None
    }
    fn event(&mut self, cx: &mut Ctx<E>, ev: &Event) -> Action<E> {
        if let Event::PhoneText(t) = ev {
            cx.phone_text.take();
            self.query = t.clone();
            self.nav.focus = 0;
            return Action::Redraw;
        }
        Action::None
    }
    fn result(&mut self, _cx: &mut Ctx<E>, r: Result_) -> Action<E> {
        if let Result_::Text(t) = r {
            self.query = t;
            self.nav.focus = 0;
        }
        Action::Redraw
    }
}

/// Browse: subjects, authors A–Z, collections, languages.
pub struct Browse {
    /// 0 = kinds, 1 = a list of groups, 2 = books.
    level: u8,
    kind: usize,
    /// Groups of `kind`, read when the kind is opened.
    groups: Vec<Group>,
    group: usize,
    /// Members of the open group.
    members: Vec<u32>,
    rows: PageRows,
    nav: ListNav,
}

impl Browse {
    /// New.
    pub fn new() -> Self {
        Browse { level: 0, kind: 0, groups: Vec::new(), group: 0, members: Vec::new(), rows: PageRows::default(), nav: ListNav::new(4, 10) }
    }
}

impl Default for Browse {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: Env> Screen<E> for Browse {
    fn name(&self) -> &'static str {
        "38-browse"
    }
    fn draw(&mut self, cx: &mut Ctx<E>, f: &mut Frame) -> Refresh {
        let title = match self.level {
            0 => String::from("Browse"),
            1 => String::from(KIND_NAMES[self.kind]),
            _ => self.groups.get(self.group).map(|g| g.name.clone()).unwrap_or_default(),
        };
        let row_h = ROW_H;
        let per = widgets::rows_between(widgets::CONTENT_TOP, f.height() as i32 - RAIL_H, row_h);
        if per != self.nav.per_page {
            self.nav.per_page = per;
            self.rows.invalidate();
        }
        running_head(f, &title, Some(&page_indicator(self.nav.page(), self.nav.pages())));
        let mut y = widgets::CONTENT_TOP;
        match self.level {
            0 => {
                let kinds = [
                    ("Subjects", "adventure, mystery, philosophy…"),
                    ("Authors A to Z", ""),
                    ("Collections", "curated sets with a line of intro"),
                    ("Languages", ""),
                ];
                self.nav.set_n(4);
                for (i, (t, s)) in kinds.iter().enumerate() {
                    row(
                        f,
                        y,
                        row_h,
                        t,
                        if s.is_empty() { None } else { Some(s) },
                        None,
                        if i == self.nav.focus { RowState::Focused } else { RowState::Normal },
                    );
                    y += row_h;
                }
            }
            1 => {
                self.nav.set_n(self.groups.len());
                for i in self.nav.visible() {
                    let g = &self.groups[i];
                    row(
                        f,
                        y,
                        row_h,
                        &g.name,
                        None,
                        Some(&alloc::format!("{}", g.count)),
                        if i == self.nav.focus { RowState::Focused } else { RowState::Normal },
                    );
                    y += row_h;
                }
                if self.groups.is_empty() {
                    empty_state(f, EMPTY_Y, "Nothing here yet", "The catalog has no entries of this kind.");
                }
            }
            _ => {
                self.nav.set_n(self.members.len());
                let start = self.nav.visible().start;
                let rows = self.rows.ensure(cx.env.fs(), &self.members, &self.nav);
                for (k, r) in rows.iter().enumerate() {
                    book_row(cx, f, y, row_h, r, start + k == self.nav.focus, false);
                    y += row_h;
                }
            }
        }
        rail(f, ["", "Back", "Open", ""], None);
        Refresh::Gc
    }
    fn key(&mut self, cx: &mut Ctx<E>, ev: KeyEvent) -> Action<E> {
        if ev.kind == KeyKind::Release {
            return Action::None;
        }
        if ev.is(Key::Back) {
            if self.level == 0 {
                return Action::Pop;
            }
            self.level -= 1;
            self.nav.focus = 0;
            self.rows.invalidate();
            return Action::Redraw;
        }
        if ev.is(Key::Confirm) {
            match self.level {
                0 => {
                    self.kind = self.nav.focus;
                    self.groups = open_catalog(cx.env.fs()).groups(self.kind);
                    self.level = 1;
                    self.nav.focus = 0;
                }
                1 => {
                    if let Some(g) = self.groups.get(self.nav.focus) {
                        self.members = open_catalog(cx.env.fs()).members(g);
                        self.group = self.nav.focus;
                        self.level = 2;
                        self.nav.focus = 0;
                        self.rows.invalidate();
                    }
                }
                _ => {
                    if let Some(i) = self.members.get(self.nav.focus) {
                        return Action::Push(Box::new(BookPage::new(*i)));
                    }
                }
            }
            return Action::Redraw;
        }
        if self.nav.key(ev) {
            return Action::Redraw;
        }
        Action::None
    }
}

// ---------------------------------------------------------------------------------------
// 39 downloads and saved

/// The download queue and the saved list.
pub struct Downloads {
    nav: ListNav,
    saved_tab: bool,
    /// The saved list's rows, read once per visit of the tab.
    saved: Option<Vec<Row>>,
}

impl Downloads {
    /// New.
    pub fn new() -> Self {
        Downloads { nav: ListNav::new(0, 9), saved_tab: false, saved: None }
    }
    fn saved_rows<E: Env>(&mut self, cx: &Ctx<E>) -> &[Row] {
        if self.saved.is_none() {
            let fs = cx.env.fs();
            let ids = load_saved(fs);
            let cat = open_catalog(fs);
            let idx = cat.find_ids(&ids);
            self.saved = Some(cat.rows(&idx));
        }
        self.saved.as_deref().unwrap_or(&[])
    }
}

impl Default for Downloads {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: Env> Screen<E> for Downloads {
    fn name(&self) -> &'static str {
        "39-downloads"
    }
    fn draw(&mut self, cx: &mut Ctx<E>, f: &mut Frame) -> Refresh {
        let y0 = widgets::tabs(f, widgets::CONTENT_TOP - 8, &["Downloads", "Saved"], self.saved_tab as usize, false, None);
        running_head(f, "Downloads", None);
        let row_h = ROW_H;
        self.nav.per_page = widgets::rows_between(y0 + 4, f.height() as i32 - RAIL_H, row_h);
        let mut y = y0 + 4;
        if self.saved_tab {
            let focus = self.nav.focus;
            let rows: Vec<Row> = self.saved_rows(cx).to_vec();
            self.nav.set_n(rows.len());
            if rows.is_empty() {
                empty_state(f, EMPTY_Y, "Nothing saved", "Left on a book page saves it for later.");
            }
            for i in self.nav.visible() {
                book_row(cx, f, y, row_h, &rows[i], i == focus, false);
                y += row_h;
            }
            rail(f, ["Queue", "Back", if rows.is_empty() { "" } else { "Open" }, if rows.is_empty() { "" } else { "Get all" }], None);
        } else {
            let downloads = cx.env.net().downloads();
            self.nav.set_n(downloads.len());
            if downloads.is_empty() {
                empty_state(f, EMPTY_Y, "No downloads", "Books you get from the shop appear here.");
                rail(f, ["Saved", "Back", "Bookshop", ""], None);
                return Refresh::Gc;
            }
            for i in self.nav.visible() {
                let d = &downloads[i];
                let v = match &d.state {
                    DownloadState::Queued => String::from("waiting"),
                    DownloadState::Working => {
                        d.total.map(|t| alloc::format!("{}%", d.done * 100 / t.max(1))).unwrap_or_else(|| String::from("…"))
                    }
                    DownloadState::Done => String::from("done"),
                    DownloadState::Failed(e) => ellipsis(quire_fonts::ui::label(), e, 160),
                    DownloadState::Retrying(s) => alloc::format!("retry in {s} s"),
                };
                row(
                    f,
                    y,
                    row_h,
                    &d.title,
                    Some(&d.author),
                    Some(&v),
                    if i == self.nav.focus { RowState::Focused } else { RowState::Normal },
                );
                y += row_h;
            }
            rail(f, ["Saved", "Back", "Retry", "Cancel"], None);
        }
        Refresh::Gc
    }
    fn key(&mut self, cx: &mut Ctx<E>, ev: KeyEvent) -> Action<E> {
        if ev.kind != KeyKind::Press {
            return Action::None;
        }
        match ev.key {
            Key::Back => Action::Pop,
            Key::Left => {
                self.saved_tab = !self.saved_tab;
                self.nav.focus = 0;
                if self.saved_tab {
                    self.saved = None;
                }
                Action::Redraw
            }
            Key::Confirm if self.saved_tab => {
                let focus = self.nav.focus;
                match self.saved_rows(cx).get(focus) {
                    Some(r) => Action::Push(Box::new(BookPage::new(r.index))),
                    None => Action::None,
                }
            }
            Key::Confirm => {
                if cx.env.net().downloads().is_empty() {
                    return Action::Push(Box::new(BookshopHome::new()));
                }
                if let Some(d) = cx.env.net().downloads().get(self.nav.focus) {
                    let url = d.url.clone();
                    cx.env.request(SysRequest::Fetch(FetchRequest::Retry(url)));
                }
                Action::Redraw
            }
            Key::Right if self.saved_tab => {
                let rows: Vec<Row> = self.saved_rows(cx).to_vec();
                let cat = open_catalog(cx.env.fs());
                for r in rows {
                    if in_library(cx, &r.title, &r.author).is_none() {
                        if let Some(b) = cat.book(r.index) {
                            cx.env.request(SysRequest::Fetch(FetchRequest::Book {
                                url: b.url,
                                title: b.title,
                                author: b.author,
                                size: Some(b.size as u64),
                            }));
                        }
                    }
                }
                self.saved_tab = false;
                self.nav.focus = 0;
                Action::Redraw
            }
            Key::Right => {
                if cx.env.net().downloads().is_empty() {
                    return Action::None;
                }
                if let Some(d) = cx.env.net().downloads().get(self.nav.focus) {
                    let url = d.url.clone();
                    cx.env.request(SysRequest::Fetch(FetchRequest::Cancel(url)));
                }
                Action::Redraw
            }
            _ => {
                if self.nav.key(ev) {
                    Action::Redraw
                } else {
                    Action::None
                }
            }
        }
    }
    fn event(&mut self, _cx: &mut Ctx<E>, ev: &Event) -> Action<E> {
        if matches!(ev, Event::Net(NetEvent::Downloads)) {
            Action::Redraw
        } else {
            Action::None
        }
    }
}

// ---------------------------------------------------------------------------------------
// 32 OPDS

/// OPDS catalogs: saved servers, browser, download.
pub struct OpdsScreen {
    /// Current feed: (title, entries), or None at the server list.
    feed: Option<(String, Vec<OpdsEntry>)>,
    history: Vec<String>,
    loading: Option<String>,
    error: Option<String>,
    nav: ListNav,
}

impl OpdsScreen {
    /// New.
    pub fn new() -> Self {
        OpdsScreen { feed: None, history: Vec::new(), loading: None, error: None, nav: ListNav::new(0, 9) }
    }
}

impl Default for OpdsScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: Env> Screen<E> for OpdsScreen {
    fn name(&self) -> &'static str {
        "32-opds"
    }
    fn draw(&mut self, cx: &mut Ctx<E>, f: &mut Frame) -> Refresh {
        let title = self.feed.as_ref().map(|f| f.0.clone()).unwrap_or_else(|| String::from("Catalogs"));
        let row_h = ROW_H;
        self.nav.per_page = widgets::rows_between(widgets::CONTENT_TOP, f.height() as i32 - RAIL_H, row_h);
        running_head(f, &title, Some(&page_indicator(self.nav.page(), self.nav.pages())));
        let mut y = widgets::CONTENT_TOP;
        if let Some(l) = &self.loading {
            widgets::working_card(f, "Loading", l, 0, "fetching the catalog");
            rail(f, ["", "Back", "", ""], None);
            return Refresh::Du;
        }
        if let Some(e) = &self.error {
            let fb = quire_fonts::ui::body();
            for l in wrap(fb, &alloc::format!("Couldn't load the catalog. {e}"), f.width() as i32 - 2 * widgets::INSET) {
                draw_text(f, fb, widgets::INSET, y + fb.ascent(), &l, TextStyle::INK);
                y += line_h(fb);
            }
            y += 12;
        }
        match &self.feed {
            None => {
                let servers = &cx.settings.opds;
                let n_saved = servers.len();
                self.nav.set_n(n_saved + 1);
                if n_saved > 0 && self.nav.page() == 0 {
                    y = widgets::section_label(f, y, "Saved");
                }
                for i in self.nav.visible() {
                    if i < n_saved {
                        row(
                            f,
                            y,
                            row_h,
                            &servers[i].0,
                            Some(&servers[i].1),
                            None,
                            if i == self.nav.focus { RowState::Focused } else { RowState::Normal },
                        );
                    } else {
                        row(
                            f,
                            y,
                            row_h,
                            "Add a catalog",
                            Some("URL of an OPDS feed"),
                            None,
                            if i == self.nav.focus { RowState::Focused } else { RowState::Normal },
                        );
                    }
                    y += row_h;
                }
                let on_add = self.nav.focus == n_saved;
                rail(f, ["", "Back", if on_add { "Add" } else { "Open" }, if on_add { "" } else { "Remove" }], None);
            }
            Some((_, entries)) => {
                self.nav.set_n(entries.len());
                for i in self.nav.visible() {
                    let e = &entries[i];
                    let v = if e.nav.is_some() { "›" } else { "Get" };
                    row(
                        f,
                        y,
                        row_h,
                        &e.title,
                        if e.author.is_empty() { None } else { Some(&e.author) },
                        Some(v),
                        if i == self.nav.focus { RowState::Focused } else { RowState::Normal },
                    );
                    y += row_h;
                }
                if entries.is_empty() {
                    empty_state(f, EMPTY_Y, "Empty catalog", "");
                }
                rail(f, ["", "Back", if entries.is_empty() { "" } else { "Open" }, ""], None);
            }
        }
        Refresh::Gc
    }
    fn key(&mut self, cx: &mut Ctx<E>, ev: KeyEvent) -> Action<E> {
        if ev.kind == KeyKind::Release {
            return Action::None;
        }
        if ev.is(Key::Back) {
            if self.feed.is_some() {
                match self.history.pop() {
                    Some(prev) => {
                        self.loading = Some(prev.clone());
                        cx.env.request(SysRequest::Fetch(FetchRequest::Opds(prev)));
                    }
                    None => self.feed = None,
                }
                self.nav.focus = 0;
                return Action::Redraw;
            }
            return Action::Pop;
        }
        if ev.is(Key::Confirm) {
            match &self.feed {
                None => {
                    let servers = &cx.settings.opds;
                    if self.nav.focus >= servers.len() {
                        return Action::Push(KeyboardScreen::new("Catalog URL", "https://", "https://…/opds").boxed());
                    }
                    if !matches!(cx.env.wifi(), WifiState::Connected { .. }) {
                        return Action::Push(Box::new(super::wifi::WifiScreen::new_with_hint("Catalogs need Wi-Fi")));
                    }
                    let url = servers[self.nav.focus].1.clone();
                    self.history.clear();
                    self.loading = Some(url.clone());
                    self.error = None;
                    cx.env.request(SysRequest::Fetch(FetchRequest::Opds(url)));
                }
                Some((cur, entries)) => {
                    let Some(e) = entries.get(self.nav.focus).cloned() else { return Action::None };
                    if let Some(n) = e.nav {
                        self.history.push(cur.clone());
                        self.loading = Some(n.clone());
                        cx.env.request(SysRequest::Fetch(FetchRequest::Opds(n)));
                    } else if let Some(a) = e.acquisition {
                        cx.env.request(SysRequest::Fetch(FetchRequest::Book {
                            url: a,
                            title: e.title.clone(),
                            author: e.author.clone(),
                            size: None,
                        }));
                        return Action::Push(Box::new(Downloads::new()));
                    }
                }
            }
            return Action::Redraw;
        }
        if ev.is(Key::Right) && self.feed.is_none() {
            if self.nav.focus < cx.settings.opds.len() {
                cx.settings.opds.remove(self.nav.focus);
            }
            return Action::Redraw;
        }
        if self.nav.key(ev) {
            return Action::Redraw;
        }
        Action::None
    }
    fn event(&mut self, _cx: &mut Ctx<E>, ev: &Event) -> Action<E> {
        if let Event::Net(NetEvent::Opds(r)) = ev {
            self.loading = None;
            match r {
                Ok((t, entries)) => {
                    self.feed = Some((t.clone(), entries.clone()));
                    self.error = None;
                }
                Err(e) => self.error = Some(e.clone()),
            }
            self.nav.focus = 0;
            return Action::Redraw;
        }
        Action::None
    }
    fn result(&mut self, cx: &mut Ctx<E>, r: Result_) -> Action<E> {
        if let Result_::Text(url) = r {
            let url = url.trim();
            if url.starts_with("http") && cx.settings.opds.len() < 16 {
                let name = url.trim_start_matches("https://").trim_start_matches("http://").split('/').next().unwrap_or("catalog").into();
                cx.settings.opds.push((name, String::from(url)));
            }
        }
        Action::Redraw
    }
}

// ---------------------------------------------------------------------------------------
// 33 Calibre connect, 34 sync

/// Calibre wireless device connection status.
pub struct CalibreScreen {
    started: bool,
}

impl CalibreScreen {
    /// New.
    pub fn new() -> Self {
        CalibreScreen { started: false }
    }
}

impl Default for CalibreScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: Env> Screen<E> for CalibreScreen {
    fn name(&self) -> &'static str {
        "33-calibre"
    }
    fn draw(&mut self, cx: &mut Ctx<E>, f: &mut Frame) -> Refresh {
        if !self.started {
            self.started = true;
            cx.env.request(SysRequest::Calibre(true));
        }
        running_head(f, "Calibre", None);
        let w = f.width() as i32;
        let fb = quire_fonts::ui::body();
        let fl = quire_fonts::ui::label();
        let mut y = widgets::CONTENT_TOP;
        let WifiState::Connected { ip, .. } = cx.env.wifi() else {
            // Nothing to wait for without a network: the rail carries the fix.
            empty_state(f, EMPTY_Y, "Calibre needs Wi-Fi", "Turn it on, then start the wireless device connection in Calibre.");
            rail(f, ["", "Back", "Wi-Fi", ""], None);
            return Refresh::Gc;
        };
        for l in wrap(fb, &alloc::format!("Waiting for Calibre… {ip}:{}", cx.settings.calibre_port), w - 2 * widgets::INSET) {
            draw_text(f, fb, widgets::INSET, y + fb.ascent(), &l, TextStyle::INK);
            y += line_h(fb);
        }
        y += 8;
        let status = cx.env.net().calibre_status();
        draw_text(f, fl, widgets::INSET, y + fl.ascent(), &ellipsis(fl, &status, w - 2 * widgets::INSET), TextStyle::INK);
        y += line_h(fl) + 16;
        for l in wrap(
            fb,
            "In Calibre choose Connect/share → Start wireless device connection, then send books to the device.",
            w - 2 * widgets::INSET,
        ) {
            draw_text(f, fb, widgets::INSET, y + fb.ascent(), &l, TextStyle::INK);
            y += line_h(fb);
        }
        y += 12;
        let downloads = cx.env.net().downloads();
        for d in downloads.iter().filter(|d| d.url.starts_with("calibre:")).rev().take(6) {
            let v = match &d.state {
                DownloadState::Done => String::from("added"),
                DownloadState::Working => d.total.map(|t| alloc::format!("{}%", d.done * 100 / t.max(1))).unwrap_or_default(),
                DownloadState::Failed(e) => ellipsis(fl, e, 160),
                _ => String::from("…"),
            };
            row(f, y, ROW_H, &d.title, Some(&d.author), Some(&v), RowState::Normal);
            y += ROW_H;
        }
        rail(f, ["", "Back", "", ""], None);
        Refresh::Gc
    }
    fn key(&mut self, cx: &mut Ctx<E>, ev: KeyEvent) -> Action<E> {
        if ev.is(Key::Back) {
            cx.env.request(SysRequest::Calibre(false));
            return Action::Pop;
        }
        if ev.is(Key::Confirm) && !matches!(cx.env.wifi(), WifiState::Connected { .. }) {
            return Action::Push(Box::new(super::wifi::WifiScreen::new_with_hint("Calibre needs Wi-Fi")));
        }
        Action::None
    }
    fn event(&mut self, _cx: &mut Ctx<E>, ev: &Event) -> Action<E> {
        match ev {
            Event::Net(_) | Event::Wifi(_) | Event::BooksChanged => Action::Redraw,
            _ => Action::None,
        }
    }
}

/// Position sync (KOReader-compatible server).
pub struct SyncScreen {
    focus: usize,
    last: Option<Result<u32, String>>,
    working: bool,
}

impl SyncScreen {
    /// New.
    pub fn new() -> Self {
        SyncScreen { focus: 0, last: None, working: false }
    }
}

impl Default for SyncScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: Env> Screen<E> for SyncScreen {
    fn name(&self) -> &'static str {
        "34-sync"
    }
    fn draw(&mut self, cx: &mut Ctx<E>, f: &mut Frame) -> Refresh {
        running_head(f, "Sync", None);
        let s = &*cx.settings;
        let mut y = widgets::CONTENT_TOP;
        let status = match &self.last {
            None if self.working => String::from("syncing…"),
            None => String::from("never"),
            Some(Ok(n)) => alloc::format!("{n} books updated"),
            Some(Err(e)) => ellipsis(quire_fonts::ui::label(), e, 200),
        };
        let rows: [(&str, SettingValue); 4] = [
            ("Sync now", SettingValue::Text(status)),
            ("Server", SettingValue::Text(if s.sync_url.is_empty() { String::from("not set") } else { s.sync_url.clone() })),
            ("User", SettingValue::Text(s.sync_user.clone())),
            ("Key", SettingValue::Text(if s.sync_key.is_empty() { String::new() } else { String::from("••••") })),
        ];
        for (i, (t, v)) in rows.iter().enumerate() {
            setting_row(f, y, ROW_H, t, v, if self.focus == i { RowState::Focused } else { RowState::Normal });
            y += ROW_H;
        }
        y += 16;
        let fl = quire_fonts::ui::label();
        for l in wrap(fl, "Positions sync per book with a KOReader-compatible progress server. Books match by file hash. Your reading stays on your server.", f.width() as i32 - 2 * widgets::INSET) {
            draw_text(f, fl, widgets::INSET, y + fl.ascent(), &l, TextStyle::INK);
            y += line_h(fl);
        }
        rail(f, ["", "Back", if self.focus == 0 { "Sync" } else { "Change" }, ""], None);
        Refresh::Gc
    }
    fn key(&mut self, cx: &mut Ctx<E>, ev: KeyEvent) -> Action<E> {
        if ev.kind != KeyKind::Press {
            return Action::None;
        }
        match ev.key {
            Key::Back => Action::Pop,
            Key::Up => {
                self.focus = (self.focus + 3) % 4;
                Action::Redraw
            }
            Key::Down => {
                self.focus = (self.focus + 1) % 4;
                Action::Redraw
            }
            Key::Confirm | Key::Right => match self.focus {
                0 => {
                    if !matches!(cx.env.wifi(), WifiState::Connected { .. }) {
                        return Action::Push(Box::new(super::wifi::WifiScreen::new_with_hint("Sync needs Wi-Fi")));
                    }
                    self.working = true;
                    self.last = None;
                    cx.env.request(SysRequest::SyncNow);
                    Action::Redraw
                }
                1 => Action::Push(KeyboardScreen::new("Sync server", &cx.settings.sync_url, "https://sync.example.org").boxed()),
                2 => Action::Push(KeyboardScreen::new("Sync user", &cx.settings.sync_user, "user").boxed()),
                _ => Action::Push(KeyboardScreen::new("Sync key", "", "password").secret().boxed()),
            },
            _ => Action::None,
        }
    }
    fn event(&mut self, _cx: &mut Ctx<E>, ev: &Event) -> Action<E> {
        if let Event::Net(NetEvent::Sync(r)) = ev {
            self.working = false;
            self.last = Some(r.clone());
            return Action::Redraw;
        }
        Action::None
    }
    fn result(&mut self, cx: &mut Ctx<E>, r: Result_) -> Action<E> {
        if let Result_::Text(t) = r {
            match self.focus {
                1 => cx.settings.sync_url = t.trim().into(),
                2 => cx.settings.sync_user = t.trim().into(),
                3 => cx.settings.sync_key = t,
                _ => {}
            }
        }
        Action::Redraw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_roundtrips_through_the_fixed_record_file() {
        let cat = seed();
        let bytes = encode(&cat);
        let file = CatalogFile::parse(bytes).expect("parses");
        assert_eq!(file.count(), cat.books.len() as u32);
        // Records are in rank order and decode back to the book.
        let first = file.book(0).unwrap();
        assert_eq!(first.id, "pg1342");
        assert_eq!(first.blurb, cat.books[0].blurb);
        assert_eq!(first.subjects, cat.books[0].subjects);
        assert_eq!(first.url, cat.books[0].url);
        let row = file.row(3).unwrap();
        assert_eq!(row.title, "Moby-Dick");
        assert_eq!(row.author, "Herman Melville");
        assert_eq!(row.hours10, 180);
        assert!(file.row(999).is_none());
        // Shelves and groups.
        assert_eq!(file.shelf(0).len(), 12, "start here (all seed books are in the collection, capped)");
        assert_eq!(file.shelf(1)[0], 0);
        let authors = file.groups(1);
        assert!(authors.iter().any(|g| g.name == "Charles Dickens" && g.count == 2));
        let dickens = authors.iter().find(|g| g.name == "Charles Dickens").unwrap();
        let members = file.members(dickens);
        assert_eq!(members.len(), 2);
        assert!(members.iter().all(|i| file.row(*i).unwrap().author == "Charles Dickens"));
        let langs = file.groups(3);
        assert_eq!(langs.len(), 1);
        assert_eq!(langs[0].name, "EN");
        // Search and id lookup.
        let hits = file.search("dickens");
        assert_eq!(hits.len(), 2);
        assert!(file.search("MYSTERY holmes").contains(&1));
        assert!(file.search("zzzz").is_empty());
        let found = file.find_ids(&[String::from("pg205"), String::from("nope"), String::from("pg84")]);
        assert_eq!(found, alloc::vec![2, 4]);
    }

    #[test]
    fn long_fields_are_cut_at_character_boundaries() {
        let mut cat = seed();
        cat.books[0].title = "Ééééééééééééééééééééééééééééééééééééééééééééééééééé long".into();
        let file = CatalogFile::parse(encode(&cat)).unwrap();
        let row = file.row(0).unwrap();
        assert!(row.title.len() <= 56);
        assert!(row.title.starts_with("Éé"));
        // The full title survives in the strings blob.
        assert_eq!(file.book(0).unwrap().title, cat.books[0].title);
    }

    #[test]
    fn sizes_read_as_megabytes() {
        assert_eq!(fmt_mb(0), "—");
        assert_eq!(fmt_mb(720_000), "0.7 MB");
        assert_eq!(fmt_mb(1_400_000), "1.4 MB");
        assert_eq!(fmt_mb(33_000_000), "33 MB");
    }
}
