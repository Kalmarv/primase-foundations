//! Many stored entries of a few large files, read in the files' own order: the whole-archive pass of a census, an index
//! build or an extraction.
//!
//! An archive reader's one-entry read opens its container, seeks and reads. Called for every entry in the index's order
//! (a hash order, so a random walk over the file), that is one small read at a time through the system's file cache.
//! [`scan`] takes every wanted entry at once as an [`Extent`] (a file, an offset, a length), sorts them by file and
//! offset and joins neighbours into windows of up to [`Options::window`] bytes, leaving a gap of at most
//! [`Options::max_gap`] unrequested bytes between two joined entries. Threads take windows in turn: each reads its window
//! in one call and hands every entry in it to `visit` on the same thread, while the others read. The format's own work
//! (decryption, decompression, parsing) goes in `visit`, so it overlaps the reading of the next windows; entries arrive
//! in no particular order.
//!
//! On Windows the files are opened unbuffered (`FILE_FLAG_NO_BUFFERING`): the bytes go from the drive into the window's
//! buffer without a copy through the file cache, and a pass over tens of gigabytes does not evict the rest of the cache.
//! Windows then start and end on 4096-byte boundaries, which covers 512-byte and 4K-native sectors; a file the system
//! refuses to read that way is read again buffered. Elsewhere reads are buffered.
//!
//! An extent longer than [`Options::window`] is a read of its own, a large one. The large reads go first, longest first,
//! so that a very large entry does not start last and leave one thread working alone; but the large reads in flight
//! hold at most `threads` times four windows between them, and one longer than that is read only while no other large
//! read is held. A thread that cannot take the next large read takes ordinary windows meanwhile. Each thread keeps one
//! window's buffer for its ordinary reads and frees a large read's buffer once `visit` has had its extent. So the scan
//! holds about `threads` times [`Options::window`], plus the larger of `threads` times four windows and its largest
//! extent, however many large entries there are. A failed read or an extent that runs past the end of its file reaches `visit` as that extent's
//! [`ExtentError`]; the scan goes on with the other extents. A panic in `visit` stops the scan (the other threads take no
//! new window) and is returned as [`Panicked`], as in [`crate::parallel`].
//!
//! This module reads files: it opts out of the crate's no-file-access rule (CONTRIBUTING.md, Conventions) in the
//! functions that open and read, and only there.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use crate::parallel::{Panicked, Threads};

/// The alignment of an unbuffered read's offset, length and buffer.
const ALIGN: u64 = 4096;
/// [`ALIGN`] as a buffer offset.
const ALIGN_BYTES: usize = 4096;
/// Clears the bits below [`ALIGN`]: an offset rounded down to it.
const ALIGN_DOWN: u64 = !(ALIGN - 1);

/// One stored entry: `len` bytes at `offset` of `files[file]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
    /// The index of the entry's file in the `files` passed to [`scan`].
    pub file: usize,
    /// Where the entry starts in its file.
    pub offset: u64,
    /// Its stored length in bytes.
    pub len: usize,
}

/// How [`scan`] reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// The threads that read and visit.
    pub threads: Threads,
    /// The most bytes one read covers when it joins several extents; a longer extent is read alone, whole.
    pub window: u64,
    /// The most unrequested bytes between two extents one read joins. A larger gap starts a new read, so a sparse
    /// selection does not read the bytes between its entries.
    pub max_gap: u64,
}

impl Options {
    /// 16 MiB windows joining extents up to 1 MiB apart, on `threads` threads. On a 6 GB/s NVMe drive, reading a 50 GB
    /// archive this way reaches the drive's own rate with 16 threads.
    pub fn new(threads: Threads) -> Options {
        Options {
            threads,
            window: 16 << 20,
            max_gap: 1 << 20,
        }
    }
}

/// Why an extent's bytes could not be given to `visit`.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ExtentError {
    /// The extent's file could not be opened or read.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// What the system reported; shared by every extent of the failed read.
        source: Arc<std::io::Error>,
    },
    /// The extent runs past the end of its file.
    #[error("{path}: {len} bytes at {offset:#x} run past the end of the file at {end:#x}")]
    PastEnd {
        /// The file.
        path: PathBuf,
        /// The extent's offset.
        offset: u64,
        /// The extent's length.
        len: usize,
        /// Where the file ends.
        end: u64,
    },
    /// The extent names a file `files` does not have.
    #[error("file {file} of {files}")]
    NoFile {
        /// The index the extent names.
        file: usize,
        /// How many files there are.
        files: usize,
    },
}

/// What a [`scan`] read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scanned {
    /// How many reads it made.
    pub windows: u64,
    /// The bytes those reads returned, alignment and gaps included.
    pub bytes: u64,
}

/// One read: a span of one file and the extents inside it, by index.
#[derive(Debug)]
struct Window {
    file: usize,
    start: u64,
    end: u64,
    extents: Vec<usize>,
}

/// The extents grouped into reads ([`windows`]).
#[derive(Debug, Default)]
struct Plan {
    /// The reads of at most [`Options::window`] bytes, in file order.
    ordinary: Vec<Window>,
    /// The reads of one extent longer than a window, longest first.
    large: Vec<Window>,
    /// The extents that cannot be read (a missing file, an end past `u64`), with why.
    bad: Vec<(usize, ExtentError)>,
}

impl Window {
    /// The bytes the read covers.
    fn span(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

/// The extents grouped into reads: neighbours joined into ordinary windows in file order, and each extent longer than a
/// window a large read of its own.
fn windows(files: usize, extents: &[Extent], options: &Options) -> Plan {
    let mut bad = Vec::new();
    let mut order: Vec<(usize, u64, u64, usize)> = Vec::with_capacity(extents.len());
    for (i, e) in extents.iter().enumerate() {
        if e.file >= files {
            bad.push((i, ExtentError::NoFile { file: e.file, files }));
            continue;
        }
        let end = u64::try_from(e.len)
            .ok()
            .and_then(|len| e.offset.checked_add(len));
        let aligned = end.and_then(|end| end.checked_next_multiple_of(ALIGN));
        match aligned {
            Some(end) => order.push((e.file, e.offset & ALIGN_DOWN, end, i)),
            // an end past u64 is past the end of any file
            None => bad.push((
                i,
                ExtentError::PastEnd {
                    path: PathBuf::new(),
                    offset: e.offset,
                    len: e.len,
                    end: u64::MAX,
                },
            )),
        }
    }
    order.sort_unstable();
    let mut out: Vec<Window> = Vec::new();
    for (file, start, end, i) in order {
        if let Some(w) = out.last_mut()
            && w.file == file
            && start <= w.end.saturating_add(options.max_gap)
            && end.max(w.end).saturating_sub(w.start) <= options.window
        {
            w.end = w.end.max(end);
            w.extents.push(i);
            continue;
        }
        out.push(Window {
            file,
            start,
            end,
            extents: vec![i],
        });
    }
    let (mut large, ordinary): (Vec<Window>, Vec<Window>) =
        out.into_iter().partition(|w| w.span() > options.window);
    large.sort_by_key(|w| (std::cmp::Reverse(w.span()), w.file, w.start));
    Plan { ordinary, large, bad }
}

/// The large reads ([`Plan::large`]) taken so far and the bytes of those whose extent `visit` has not finished with.
#[derive(Debug, Default)]
struct Held {
    next: usize,
    bytes: u64,
}

/// One read a worker takes: an ordinary window, or a large one whose bytes [`Held`] counts until it is released.
enum Job<'w> {
    Ordinary(&'w Window),
    Large(&'w Window),
}

/// A buffer whose usable part starts on an [`ALIGN`] boundary, kept by one thread and grown as its windows need.
#[derive(Debug, Default)]
struct Aligned(Vec<u8>);

impl Aligned {
    fn get(&mut self, len: usize) -> Option<&mut [u8]> {
        let total = len.checked_add(ALIGN_BYTES)?;
        if self.0.len() < total {
            self.0 = vec![0; total];
        }
        let off = self.0.as_ptr().align_offset(ALIGN_BYTES);
        self.0.get_mut(off..off.checked_add(len)?)
    }
}

/// An open file of the scan: unbuffered where the system allows it.
#[derive(Debug)]
#[allow(
    clippy::disallowed_types,
    reason = "toolkit I/O: scan reads the files its caller names (module documentation)"
)]
struct Open {
    file: std::fs::File,
    unbuffered: bool,
}

#[allow(
    clippy::disallowed_types,
    reason = "toolkit I/O: scan reads the files its caller names (module documentation)"
)]
fn open(path: &Path, unbuffered: bool) -> std::io::Result<Open> {
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(windows)]
    if unbuffered {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
        o.custom_flags(FILE_FLAG_NO_BUFFERING);
    }
    Ok(Open {
        file: o.open(path)?,
        unbuffered: cfg!(windows) && unbuffered,
    })
}

#[allow(
    clippy::disallowed_types,
    reason = "toolkit I/O: scan reads the files its caller names (module documentation)"
)]
fn read_at(file: &std::fs::File, buf: &mut [u8], at: u64) -> std::io::Result<usize> {
    #[cfg(windows)]
    {
        std::os::windows::fs::FileExt::seek_read(file, buf, at)
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::read_at(file, buf, at)
    }
}

/// Fills `buf` from `at` until it is full or the file ends; the count read. An unbuffered read is whole or stops at the
/// end of the file, so a count that is not a multiple of [`ALIGN`] is the end (a further read from there would be
/// refused as unaligned).
fn fill(f: &Open, buf: &mut [u8], at: u64) -> std::io::Result<usize> {
    let mut got = 0usize;
    while let Some(rest) = buf.get_mut(got..) {
        if rest.is_empty() {
            break;
        }
        let pos = u64::try_from(got).ok().and_then(|g| at.checked_add(g));
        let Some(pos) = pos else { break };
        let n = read_at(&f.file, rest, pos)?;
        if n == 0 {
            break;
        }
        got = got.saturating_add(n);
        if f.unbuffered && !(n as u64).is_multiple_of(ALIGN) {
            break;
        }
    }
    Ok(got)
}

/// Reads every extent and calls `visit(index, bytes)` with its stored bytes (mutable, for decryption in place), or with
/// why they could not be read, once per extent, from the thread that read it. `files` are the files extents name by
/// index. See the module documentation for the order of reads and what a thread holds.
pub fn scan(
    files: &[PathBuf],
    extents: &[Extent],
    options: &Options,
    visit: impl Fn(usize, Result<&mut [u8], ExtentError>) + Sync,
) -> Result<Scanned, Panicked> {
    let Plan { ordinary, large, bad } = windows(files.len(), extents, options);
    // the large reads held at once: four windows a thread, or one read alone where it is longer
    let budget = options
        .window
        .saturating_mul(4)
        .saturating_mul(options.threads.get() as u64);
    let call = |i: usize, r: Result<&mut [u8], ExtentError>| {
        catch_unwind(AssertUnwindSafe(|| visit(i, r))).map_err(|p| crate::parallel::panicked(i, &*p))
    };
    for (i, e) in bad {
        let e = match e {
            ExtentError::PastEnd { offset, len, end, .. } => ExtentError::PastEnd {
                path: extents
                    .get(i)
                    .and_then(|x| files.get(x.file))
                    .cloned()
                    .unwrap_or_default(),
                offset,
                len,
                end,
            },
            e => e,
        };
        call(i, Err(e))?;
    }
    let next = AtomicUsize::new(0);
    let held = Mutex::new(Held::default());
    let released = Condvar::new();
    let failed = AtomicBool::new(false);
    let (windows_read, bytes_read) = (AtomicU64::new(0), AtomicU64::new(0));
    // the next read for a worker: the next large one where the held bytes allow it, else the next ordinary window, else
    // a wait for a large read to be released; `None` when every read is taken or the scan failed
    let take = || {
        let mut h = held.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if failed.load(Ordering::Relaxed) {
                return None;
            }
            if let Some(w) = large.get(h.next)
                && (h.bytes == 0 || h.bytes.saturating_add(w.span()) <= budget)
            {
                h.next = h.next.saturating_add(1);
                h.bytes = h.bytes.saturating_add(w.span());
                return Some(Job::Large(w));
            }
            drop(h);
            if let Some(w) = ordinary.get(next.fetch_add(1, Ordering::Relaxed)) {
                return Some(Job::Ordinary(w));
            }
            h = held.lock().unwrap_or_else(PoisonError::into_inner);
            if h.next >= large.len() {
                return None;
            }
            let fits = large
                .get(h.next)
                .is_some_and(|w| h.bytes == 0 || h.bytes.saturating_add(w.span()) <= budget);
            if !fits {
                h = released.wait(h).unwrap_or_else(PoisonError::into_inner);
            }
        }
    };
    let release = |w: &Window| {
        let mut h = held.lock().unwrap_or_else(PoisonError::into_inner);
        h.bytes = h.bytes.saturating_sub(w.span());
        drop(h);
        released.notify_all();
    };
    // one read into `buf` and its extents handed to `visit`; a panic in `visit` is the error
    let read_and_visit = |w: &Window,
                          buf: &mut Aligned,
                          opened: &mut Vec<Option<Result<Open, Arc<std::io::Error>>>>|
     -> Result<(), Panicked> {
        let path = files.get(w.file).map(PathBuf::as_path).unwrap_or(Path::new(""));
        let slot = opened.get_mut(w.file);
        let f = match slot {
            Some(s) => s.get_or_insert_with(|| open(path, true).map_err(Arc::new)),
            None => return Ok(()),
        };
        let span = usize::try_from(w.end.saturating_sub(w.start))
            .ok()
            .and_then(|n| buf.get(n));
        let read = match (f.as_ref(), span) {
            (Ok(f), Some(span)) => match fill(f, span, w.start) {
                // an unbuffered read the system refuses (an unusual sector size): read this file buffered
                Err(e) if f.unbuffered && e.kind() == std::io::ErrorKind::InvalidInput => {
                    match open(path, false) {
                        Ok(b) => {
                            let got = fill(&b, span, w.start);
                            if let Some(s) = opened.get_mut(w.file) {
                                *s = Some(Ok(b));
                            }
                            got.map(|n| (n, span))
                        }
                        Err(e) => Err(e),
                    }
                }
                r => r.map(|n| (n, span)),
            }
            .map_err(Arc::new),
            (Err(e), _) => Err(Arc::clone(e)),
            (Ok(_), None) => Err(Arc::new(std::io::Error::other(
                "a window larger than memory can address",
            ))),
        };
        match read {
            Ok((got, span)) => {
                windows_read.fetch_add(1, Ordering::Relaxed);
                bytes_read.fetch_add(got as u64, Ordering::Relaxed);
                let file_end = w.start.saturating_add(got as u64);
                for &i in &w.extents {
                    let Some(e) = extents.get(i) else { continue };
                    let at = usize::try_from(e.offset.saturating_sub(w.start)).unwrap_or(usize::MAX);
                    let bytes = at
                        .checked_add(e.len)
                        .filter(|&end| end <= got)
                        .and_then(|end| span.get_mut(at..end));
                    let r = bytes.ok_or_else(|| ExtentError::PastEnd {
                        path: path.to_path_buf(),
                        offset: e.offset,
                        len: e.len,
                        end: file_end,
                    });
                    call(i, r)?;
                }
            }
            Err(source) => {
                for &i in &w.extents {
                    let r = Err(ExtentError::Io {
                        path: path.to_path_buf(),
                        source: Arc::clone(&source),
                    });
                    call(i, r)?;
                }
            }
        }
        Ok(())
    };
    let worker = || -> Result<(), Panicked> {
        let mut kept = Aligned::default();
        let mut opened: Vec<Option<Result<Open, Arc<std::io::Error>>>> = Vec::new();
        opened.resize_with(files.len(), || None);
        while let Some(job) = take() {
            let result = match job {
                Job::Ordinary(w) => read_and_visit(w, &mut kept, &mut opened),
                Job::Large(w) => {
                    // a buffer of its own, freed before the read is released
                    let r = read_and_visit(w, &mut Aligned::default(), &mut opened);
                    release(w);
                    r
                }
            };
            if let Err(p) = result {
                failed.store(true, Ordering::Relaxed);
                released.notify_all();
                return Err(p);
            }
        }
        Ok(())
    };
    let workers = options
        .threads
        .get()
        .min(ordinary.len().saturating_add(large.len()))
        .max(1);
    let results: Vec<Result<(), Panicked>> = if workers == 1 {
        vec![worker()]
    } else {
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..workers).map(|_| s.spawn(worker)).collect();
            // `visit`'s panics are caught per extent, so a join cannot fail
            handles.into_iter().map(|h| h.join().unwrap_or(Ok(()))).collect()
        })
    };
    if let Some(p) = results.into_iter().filter_map(Result::err).min_by_key(|p| p.item) {
        return Err(p);
    }
    Ok(Scanned {
        windows: windows_read.load(Ordering::Relaxed),
        bytes: bytes_read.load(Ordering::Relaxed),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(window: u64, max_gap: u64) -> Options {
        Options {
            threads: Threads::ONE,
            window,
            max_gap,
        }
    }

    #[test]
    fn neighbours_join_and_a_gap_splits() {
        let e = |offset, len| Extent { file: 0, offset, len };
        // 0..100 and 5000..5100 are one aligned read apart by less than the gap; 1 MiB later starts a new read
        let extents = [e(5000, 100), e(0, 100), e(1 << 20, 10)];
        let plan = windows(1, &extents, &opts(16 << 20, 8192));
        let w = plan.ordinary;
        assert!(plan.bad.is_empty() && plan.large.is_empty());
        assert_eq!(w.len(), 2);
        assert_eq!(
            (w[0].start, w[0].end, w[0].extents.clone()),
            (0, 8192, vec![1, 0])
        );
        assert_eq!((w[1].start, w[1].end), (1 << 20, (1 << 20) + 4096));
    }

    #[test]
    fn a_window_never_exceeds_its_size_unless_one_extent_does() {
        let e = |offset, len| Extent { file: 0, offset, len };
        let extents = [e(0, 4096), e(4096, 4096), e(8192, 4096), e(12288, 40000)];
        let plan = windows(1, &extents, &opts(8192, 0));
        let spans = |w: &[Window]| w.iter().map(|w| (w.start, w.end)).collect::<Vec<_>>();
        assert_eq!(spans(&plan.ordinary), [(0, 8192), (8192, 12288)]);
        // the extent longer than a window is a large read of its own
        assert_eq!(spans(&plan.large), [(12288, 53248)]);
    }

    #[test]
    fn large_reads_go_longest_first() {
        let e = |file, offset, len| Extent { file, offset, len };
        let extents = [
            e(0, 0, 20000),
            e(0, 1 << 20, 90000),
            e(1, 0, 90000),
            e(1, 1 << 20, 50000),
            e(0, 1 << 21, 100),
        ];
        let plan = windows(2, &extents, &opts(8192, 0));
        let large: Vec<_> = plan.large.iter().map(|w| (w.file, w.start, w.span())).collect();
        // longest first; of equal spans the first file and offset first
        assert_eq!(
            large,
            [
                (0, 1 << 20, 90112),
                (1, 0, 90112),
                (1, 1 << 20, 53248),
                (0, 0, 20480)
            ]
        );
        assert_eq!(plan.ordinary.len(), 1);
    }

    #[test]
    fn files_do_not_join_and_unknown_files_are_refused() {
        let extents = [
            Extent {
                file: 1,
                offset: 0,
                len: 10,
            },
            Extent {
                file: 0,
                offset: 0,
                len: 10,
            },
            Extent {
                file: 2,
                offset: 0,
                len: 10,
            },
            Extent {
                file: 0,
                offset: u64::MAX - 1,
                len: 10,
            },
        ];
        let plan = windows(2, &extents, &opts(16 << 20, 1 << 20));
        let (w, bad) = (plan.ordinary, plan.bad);
        assert_eq!(w.iter().map(|w| w.file).collect::<Vec<_>>(), [0, 1]);
        assert!(matches!(bad[0], (2, ExtentError::NoFile { file: 2, files: 2 })));
        assert!(matches!(bad[1], (3, ExtentError::PastEnd { .. })));
    }
}
