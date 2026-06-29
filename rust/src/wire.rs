//! Wire framing: UTF-8 control lines terminated by `\n`, interleaved with raw
//! binary payloads on the SAME stream. See RUST-PORT-SPEC.md section 4.2.
//!
//! Reads go through a single `BufReader` so that reading a control line never
//! discards bytes belonging to the binary payload that follows a length line:
//! leftover bytes stay buffered and are served to the next `read_exact`.

use std::io::{self, BufRead, BufReader, Read, Write};

/// A framed connection over any read+write stream (TLS or plain TCP).
pub struct Conn<S: Read + Write> {
    inner: BufReader<S>,
    /// Outgoing bytes are accumulated here so a line + its payload flush as one.
    wbuf: Vec<u8>,
}

impl<S: Read + Write> Conn<S> {
    pub fn new(stream: S) -> Self {
        Conn {
            inner: BufReader::new(stream),
            wbuf: Vec::with_capacity(64 * 1024),
        }
    }

    /// Read one control line. Returns `None` on a clean close (EOF with nothing
    /// buffered). A leading/trailing `\r` is stripped, as is the terminating `\n`.
    pub fn read_line(&mut self) -> io::Result<Option<String>> {
        let mut buf = Vec::new();
        let n = self.inner.read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Ok(None); // EOF, connection closed
        }
        // Strip the trailing \n and any surrounding \r.
        while matches!(buf.last(), Some(b'\n') | Some(b'\r')) {
            buf.pop();
        }
        if buf.first() == Some(&b'\r') {
            buf.remove(0);
        }
        Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
    }

    /// Read exactly `n` bytes of binary payload off the stream.
    pub fn read_exact_vec(&mut self, n: usize) -> io::Result<Vec<u8>> {
        let mut v = vec![0u8; n];
        self.inner.read_exact(&mut v)?;
        Ok(v)
    }

    /// Read exactly `buf.len()` bytes into `buf`.
    pub fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_exact(buf)
    }

    /// Queue a control line (`s` + `\n`) without flushing.
    pub fn put_line(&mut self, s: &str) {
        self.wbuf.extend_from_slice(s.as_bytes());
        self.wbuf.push(b'\n');
    }

    /// Queue raw bytes without flushing.
    pub fn put_bytes(&mut self, b: &[u8]) {
        self.wbuf.extend_from_slice(b);
    }

    /// Flush everything queued to the underlying stream.
    pub fn flush(&mut self) -> io::Result<()> {
        if !self.wbuf.is_empty() {
            self.inner.get_mut().write_all(&self.wbuf)?;
            self.wbuf.clear();
        }
        self.inner.get_mut().flush()
    }

    /// Queue a line and flush immediately (the common control-message case).
    pub fn send_line(&mut self, s: &str) -> io::Result<()> {
        self.put_line(s);
        self.flush()
    }

    /// Read exactly `n` bytes off the stream and write them to `w` (chunked, so
    /// arbitrarily large payloads use constant memory).
    pub fn copy_exact_to_writer<W: io::Write>(&mut self, mut n: u64, w: &mut W) -> io::Result<()> {
        let mut buf = [0u8; 65536];
        while n > 0 {
            let want = std::cmp::min(n, buf.len() as u64) as usize;
            self.inner.read_exact(&mut buf[..want])?;
            w.write_all(&buf[..want])?;
            n -= want as u64;
        }
        Ok(())
    }

    /// Stream up to `n` bytes from a reader into the connection (chunked + flushed),
    /// returning the number of bytes actually sent (less than `n` if `r` ends early).
    pub fn send_from_reader<R: io::Read>(&mut self, r: &mut R, n: u64) -> io::Result<u64> {
        let mut buf = vec![0u8; 1 << 20]; // 1 MiB on the heap (main-thread stack is small)
        let mut left = n;
        let mut sent = 0u64;
        while left > 0 {
            let want = std::cmp::min(left, buf.len() as u64) as usize;
            let got = r.read(&mut buf[..want])?;
            if got == 0 {
                break;
            }
            self.put_bytes(&buf[..got]);
            self.flush()?;
            left -= got as u64;
            sent += got as u64;
        }
        Ok(sent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A fake duplex stream backed by an in-memory read cursor and a write sink.
    struct Duplex {
        r: Cursor<Vec<u8>>,
        w: Vec<u8>,
    }
    impl Read for Duplex {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.r.read(buf)
        }
    }
    impl Write for Duplex {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.w.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn reads_lines_then_exact_binary() {
        // "B 1\n" line, then a 3-byte payload, then "E\n".
        let data = b"B 1\n\x01\x02\x03E\n".to_vec();
        let mut c = Conn::new(Duplex { r: Cursor::new(data), w: Vec::new() });
        assert_eq!(c.read_line().unwrap().as_deref(), Some("B 1"));
        assert_eq!(c.read_exact_vec(3).unwrap(), vec![1, 2, 3]);
        assert_eq!(c.read_line().unwrap().as_deref(), Some("E"));
        assert_eq!(c.read_line().unwrap(), None); // EOF
    }

    #[test]
    fn tolerates_crlf() {
        let data = b"OK\r\n".to_vec();
        let mut c = Conn::new(Duplex { r: Cursor::new(data), w: Vec::new() });
        assert_eq!(c.read_line().unwrap().as_deref(), Some("OK"));
    }

    #[test]
    fn writes_line_and_payload_together() {
        let mut c = Conn::new(Duplex { r: Cursor::new(Vec::new()), w: Vec::new() });
        c.put_line("R 3");
        c.put_bytes(&[9, 8, 7]);
        c.flush().unwrap();
        assert_eq!(c.inner.get_ref().w, b"R 3\n\x09\x08\x07");
    }
}
