//! A transfer that stops delivering bytes fails in minutes, not hours
//! (ruling 2026-09-15).
//!
//! `ureq` 3 budgets a body read as a total (`timeout_recv_body`), which a
//! 30 GB shard cannot use, and offers no per-read timeout; a dead connection
//! therefore blocks `read` forever. The reader is driven from a helper thread
//! and the copy waits on its chunks with a deadline, so silence for longer
//! than the stall window is an error — with what landed already written.

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
