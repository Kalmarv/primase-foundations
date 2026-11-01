//! `scan` against the plain reading of the same bytes, on synthetic files written here: every extent reaches `visit`
//! once, with exactly the bytes at its offset, whatever the threads, window and gap; an extent past the end of its file,
//! of a missing file or of a file not listed reaches it as that error; a panic in `visit` stops the scan; the large reads
//! held at once stay within four windows a thread, a longer one alone.
#![cfg(any(unix, windows))]

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use primase_foundations::parallel::Threads;
use primase_foundations::scan::{Extent, ExtentError, Options, scan};

/// The byte at `pos` of synthetic file `file`: varies within a sector and between sectors and files.
fn byte(file: usize, pos: u64) -> u8 {
    (pos.wrapping_mul(31) ^ (pos >> 11) ^ (file as u64).wrapping_mul(0x9d)) as u8
}

/// Files of the given sizes in a fresh folder of this test's own.
fn files(name: &str, sizes: &[u64]) -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("scan-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the test's folder");
    sizes
        .iter()
        .enumerate()
        .map(|(f, &n)| {
            let path = dir.join(format!("{f}.bin"));
            std::fs::write(&path, (0..n).map(|p| byte(f, p)).collect::<Vec<_>>()).expect("a synthetic file");
            path
        })
        .collect()
}

/// A deterministic spread of extents over files of `sizes`: random offsets and lengths, some ending exactly at the end
/// of their file, some overlapping, some empty.
fn extents(sizes: &[u64], n: usize) -> Vec<Extent> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    (0..n)
        .map(|i| {
            let file = (next() % sizes.len() as u64) as usize;
            let size = sizes[file];
            let offset = next() % size;
            let len = match i % 5 {
                0 => size - offset, // to the end of the file
                1 => 0,             // empty
                _ => (next() % 70_000).min(size - offset),
            } as usize;
            Extent { file, offset, len }
        })
        .collect()
}

type Seen = Vec<Vec<Result<Vec<u8>, String>>>;

fn run(paths: &[PathBuf], extents: &[Extent], options: &Options) -> Seen {
    let seen: Mutex<Seen> = Mutex::new(vec![Vec::new(); extents.len()]);
    scan(paths, extents, options, |i, r| {
        let r = r.map(|b| b.to_vec()).map_err(|e| match e {
            ExtentError::Io { .. } => "io".to_owned(),
            ExtentError::PastEnd { end, .. } => format!("past end {end}"),
            ExtentError::NoFile { file, files } => format!("no file {file} of {files}"),
        });
        seen.lock().expect("no visit panics")[i].push(r);
    })
    .expect("no visit panics");
    seen.into_inner().expect("no visit panics")
}

#[test]
fn every_extent_gets_its_bytes_once_whatever_the_threads_window_and_gap() {
    // a size off every alignment, one a multiple of 4096, one smaller than a sector
    let sizes = [1_000_003, 3 * 4096, 17];
    let paths = files("bytes", &sizes);
    let extents = extents(&sizes, 3000);
    for threads in [1, 7] {
        for (window, max_gap) in [(16 << 20, 1 << 20), (8192, 0), (1 << 20, 1 << 16), (4096, 4096)] {
            let options = Options {
                threads: Threads::new(threads).unwrap(),
                window,
                max_gap,
            };
            let seen = run(&paths, &extents, &options);
            for (i, (e, got)) in extents.iter().zip(&seen).enumerate() {
                let want: Vec<u8> = (e.offset..e.offset + e.len as u64)
                    .map(|p| byte(e.file, p))
                    .collect();
                assert_eq!(
                    got.len(),
                    1,
                    "extent {i} visited {} times ({threads} threads, window {window})",
                    got.len()
                );
                assert_eq!(
                    got[0].as_ref().unwrap(),
                    &want,
                    "extent {i} {e:?} ({threads} threads, window {window})"
                );
            }
        }
    }
}

#[test]
fn unreadable_extents_reach_visit_as_their_error() {
    let sizes = [10_000, 5000];
    let mut paths = files("errors", &sizes);
    paths.push(paths[0].with_file_name("missing.bin"));
    let extents = [
        Extent {
            file: 0,
            offset: 9_990,
            len: 10,
        }, // ends exactly at the end
        Extent {
            file: 0,
            offset: 9_990,
            len: 11,
        }, // one byte past it
        Extent {
            file: 1,
            offset: 6000,
            len: 1,
        }, // starts past it
        Extent {
            file: 2,
            offset: 0,
            len: 1,
        }, // a missing file
        Extent {
            file: 3,
            offset: 0,
            len: 1,
        }, // a file not listed
        Extent {
            file: 0,
            offset: u64::MAX,
            len: 2,
        }, // an end past u64
        Extent {
            file: 1,
            offset: 0,
            len: 5000,
        }, // the whole file
    ];
    for threads in [1, 4] {
        let options = Options::new(Threads::new(threads).unwrap());
        let seen = run(&paths, &extents, &options);
        let first: Vec<_> = seen.iter().map(|s| s[0].clone().map(|b| b.len())).collect();
        assert_eq!(
            first,
            [
                Ok(10),
                Err("past end 10000".to_owned()),
                Err("past end 5000".to_owned()),
                Err("io".to_owned()),
                Err("no file 3 of 3".to_owned()),
                Err(format!("past end {}", u64::MAX)),
                Ok(5000),
            ]
        );
        assert!(seen.iter().all(|s| s.len() == 1));
    }
}

#[test]
fn bytes_can_be_rewritten_in_place() {
    let paths = files("in-place", &[8192]);
    let extents = [
        Extent {
            file: 0,
            offset: 100,
            len: 50,
        },
        Extent {
            file: 0,
            offset: 120,
            len: 10,
        },
    ];
    let sums = Mutex::new(Vec::new());
    scan(&paths, &extents, &Options::new(Threads::ONE), |i, r| {
        let b = r.unwrap();
        // an in-place decryption: only the visit's own view changes what it then reads
        b.iter_mut().for_each(|x| *x ^= 0xff);
        sums.lock()
            .unwrap()
            .push((i, b.iter().map(|&x| u32::from(x)).sum::<u32>()));
    })
    .unwrap();
    assert_eq!(sums.into_inner().unwrap().len(), 2);
}

#[test]
fn a_panic_in_visit_stops_the_scan_and_names_the_extent() {
    let sizes = [200_000];
    let paths = files("panic", &sizes);
    let extents: Vec<_> = (0..100)
        .map(|i| Extent {
            file: 0,
            offset: i * 2000,
            len: 100,
        })
        .collect();
    for threads in [1, 4] {
        let options = Options {
            threads: Threads::new(threads).unwrap(),
            window: 4096,
            max_gap: 0,
        };
        let r = scan(&paths, &extents, &options, |i, _| {
            if i == 37 {
                panic!("extent 37 refused");
            }
        });
        let p = r.unwrap_err();
        assert_eq!((p.item, p.message.as_str()), (37, "extent 37 refused"));
    }
}

#[test]
fn reads_cover_what_was_asked_and_little_more() {
    let sizes = [4 << 20];
    let paths = files("sparse", &sizes);
    // two entries 2 MiB apart: with a 1 MiB gap limit they are two reads of one sector each, not one 2 MiB read
    let extents = [
        Extent {
            file: 0,
            offset: 0,
            len: 10,
        },
        Extent {
            file: 0,
            offset: 2 << 20,
            len: 10,
        },
    ];
    let s = scan(&paths, &extents, &Options::new(Threads::ONE), |_, r| {
        r.unwrap();
    })
    .unwrap();
    assert_eq!((s.windows, s.bytes), (2, 8192));
}

#[test]
fn large_reads_held_at_once_stay_within_four_windows_a_thread() {
    // 4 threads of 8 KiB windows: 128 KiB of large reads at once, and the 300 KB extent alone
    let (window, threads) = (8192u64, 4usize);
    let budget = window * 4 * threads as u64;
    let sizes = [2_000_000];
    let paths = files("held", &sizes);
    let mut extents: Vec<Extent> = (0..40u64)
        .map(|k| Extent {
            file: 0,
            offset: k * 45_000,
            len: 20_000 + (k as usize % 5) * 5000,
        })
        .collect();
    extents.push(Extent {
        file: 0,
        offset: 1_850_000,
        len: 150_000,
    });
    extents.extend((0..200u64).map(|k| Extent {
        file: 0,
        offset: 1_800_000 + k * 100,
        len: 50,
    }));
    let options = Options {
        threads: Threads::new(threads).unwrap(),
        window,
        max_gap: 0,
    };
    let (held, most, visited) = (AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0));
    scan(&paths, &extents, &options, |i, r| {
        let bytes = r.unwrap();
        let e = extents[i];
        assert!(
            bytes.iter().zip(e.offset..).all(|(&b, p)| b == byte(0, p)),
            "extent {i}"
        );
        visited.fetch_add(1, Ordering::Relaxed);
        if e.len as u64 > window {
            // the extent's read: its aligned span
            let span = (e.offset + e.len as u64).next_multiple_of(4096) - (e.offset & !4095);
            let now = held.fetch_add(span, Ordering::SeqCst) + span;
            most.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(5));
            held.fetch_sub(span, Ordering::SeqCst);
        }
    })
    .unwrap();
    assert_eq!(visited.load(Ordering::Relaxed), extents.len() as u64);
    let most = most.load(Ordering::SeqCst);
    // the 150 KB extent is longer than the budget and was read alone; the rest overlapped up to the budget
    assert!(
        most <= budget.max(155_648),
        "{most} bytes of large reads held at once"
    );
    assert!(most > 2 * 32_768, "the large reads overlapped: {most}");
}
