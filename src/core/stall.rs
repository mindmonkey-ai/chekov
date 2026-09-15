//! A transfer that stops delivering bytes fails in minutes, not hours
//! (ruling 2026-09-15).
//!
//! `ureq` 3 budgets a body read as a total (`timeout_recv_body`), which a
//! 30 GB shard cannot use, and offers no per-read timeout; a dead connection
//! therefore blocks `read` forever. The reader is driven from a helper thread
//! and the copy waits on its chunks with a deadline, so silence for longer
//! than the stall window is an error — with what landed already written.

use std::io::{ErrorKind, Read, Write};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::time::{Duration, Instant};

const CHUNK: usize = 64 * 1024;
/// Progress is reported at most this often, and always at the end.
const TICK: Duration = Duration::from_secs(1);
/// How far the reader may run ahead of the writer — bounded so a slow disk
/// never buffers a shard in memory.
const AHEAD: usize = 16;

/// How long silence may last, and who hears the running total.
pub struct Watch<F> {
    pub stall: Duration,
    /// Called with the bytes copied so far: at most once a second, and once
    /// at the end with the final total.
    pub on_progress: F,
}

enum Delivery {
    Bytes(Vec<u8>),
    Done,
    Failed(std::io::Error),
}

/// Copy `reader` into `out`, failing as `TimedOut` once no bytes have
/// arrived for `watch.stall`. Everything that arrived is written first.
///
/// The reader runs on a helper thread. After a stall that thread stays
/// blocked in `read` until the connection finally answers or the process
/// ends — the price of a reader with no timeout of its own; it never writes,
/// so a resumed transfer cannot race it.
pub fn copy_watched<R, W, F>(reader: R, out: &mut W, mut watch: Watch<F>) -> std::io::Result<u64>
where
    R: Read + Send + 'static,
    W: Write,
    F: FnMut(u64),
{
    let (tx, rx) = mpsc::sync_channel(AHEAD);
    std::thread::spawn(move || feed(reader, &tx));
    let mut copied = 0u64;
    let mut last_tick = Instant::now();
    loop {
        match rx.recv_timeout(watch.stall) {
            Ok(Delivery::Bytes(chunk)) => {
                out.write_all(&chunk)?;
                copied = copied.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
                if last_tick.elapsed() >= TICK {
                    last_tick = Instant::now();
                    (watch.on_progress)(copied);
                }
            }
            Ok(Delivery::Done) => {
                (watch.on_progress)(copied);
                return Ok(copied);
            }
            Ok(Delivery::Failed(error)) => return Err(error),
            Err(RecvTimeoutError::Timeout) => return Err(stalled(watch.stall, copied)),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(std::io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "the reader ended without reaching EOF",
                ));
            }
        }
    }
}

/// Read chunks until EOF, an error, or nobody listening any more.
fn feed<R: Read>(mut reader: R, tx: &SyncSender<Delivery>) {
    let mut buf = vec![0u8; CHUNK];
    loop {
        let delivery = match reader.read(&mut buf) {
            Ok(0) => Delivery::Done,
            Ok(n) => Delivery::Bytes(buf[..n].to_vec()),
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => Delivery::Failed(e),
        };
        let finished = !matches!(delivery, Delivery::Bytes(_));
        if tx.send(delivery).is_err() || finished {
            return;
        }
    }
}

fn stalled(window: Duration, copied: u64) -> std::io::Error {
    std::io::Error::new(
        ErrorKind::TimedOut,
        format!(
            "no bytes arrived for {}s ({copied} bytes landed and are kept) — \
             rerun `chekov pull` to resume",
            window.as_secs()
        ),
    )
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::{Watch, copy_watched};

    /// A reader fed by a channel: it blocks in `read` until the test sends
    /// another chunk, and reads EOF once the sender is dropped.
    struct Fed(mpsc::Receiver<Vec<u8>>, Vec<u8>);

    impl Read for Fed {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.1.is_empty() {
                match self.0.recv() {
                    Ok(chunk) => self.1 = chunk,
                    Err(_) => return Ok(0),
                }
            }
            let n = buf.len().min(self.1.len());
            buf[..n].copy_from_slice(&self.1[..n]);
            self.1.drain(..n);
            Ok(n)
        }
    }

    #[test]
    fn a_reader_that_goes_silent_fails_as_timed_out_with_what_landed_written() {
        let (tx, rx) = mpsc::channel();
        tx.send(b"hello ".to_vec()).expect("send");
        tx.send(b"world".to_vec()).expect("send");
        let mut out = Vec::new();
        let err = copy_watched(
            Fed(rx, Vec::new()),
            &mut out,
            Watch {
                stall: Duration::from_millis(200),
                on_progress: |_| {},
            },
        )
        .expect_err("the sender is held open and silent, so the copy must give up");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            err.to_string().contains("no bytes arrived for"),
            "the error says what happened and how long it waited: {err}"
        );
        assert_eq!(out, b"hello world", "everything that arrived was written");
        drop(tx);
    }

    #[test]
    fn a_steady_reader_copies_everything_and_reports_the_running_total() {
        let bytes = vec![7u8; 300_000];
        let mut out = Vec::new();
        let mut seen = Vec::new();
        let copied = copy_watched(
            Cursor::new(bytes.clone()),
            &mut out,
            Watch {
                stall: Duration::from_secs(5),
                on_progress: |done| seen.push(done),
            },
        )
        .expect("a live reader never stalls");
        assert_eq!(copied, 300_000);
        assert_eq!(out, bytes);
        assert_eq!(
            seen.last().copied(),
            Some(300_000),
            "the last report is the final total"
        );
    }

    #[test]
    fn a_read_error_comes_back_as_that_error_not_as_a_stall() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "peer went away",
                ))
            }
        }
        let mut out = Vec::new();
        let err = copy_watched(
            Broken,
            &mut out,
            Watch {
                stall: Duration::from_secs(5),
                on_progress: |_| {},
            },
        )
        .expect_err("the reader failed");
        assert_eq!(err.kind(), std::io::ErrorKind::ConnectionReset);
        assert!(err.to_string().contains("peer went away"), "{err}");
    }
}
