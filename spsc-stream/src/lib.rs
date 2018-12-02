use std::io::{self, Read, Write};

use std::sync::{Arc, Mutex};

/// Fixed-size ring buffer
pub struct CursorBuffer {
    // Underlying memory
    buf: Box<[u8]>,
    // Start of initialised region
    start: usize,
    // Length of initialised region (might wrap around to the start)
    len: usize
}

impl CursorBuffer {
    pub fn new(size: usize) -> Self {
        Self {
            buf: vec![0; size].into_boxed_slice(),
            start: 0,
            len: 0
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn is_full(&self) -> bool {
        self.len == self.buf.len()
    }

    pub fn read_from(&mut self, buf: &[u8]) -> usize {
        use std::cmp::min;

        let ringbuf_size = self.buf.len();

        // Max number of bytes we might read
        let buf_size = min(buf.len(), ringbuf_size - self.len);

        // The index into our ring buffer where we will start writing.
        // This is at the end of the region containing data and will wrap as that might.
        let write_start = (self.start + self.len) % ringbuf_size;

        if write_start < self.start {
            // The data in the ring wraps and we're writing to the "start" of the buffer.
            // The most we can write is the remaining free space in the buffer, i.e. until
            // we get back to the start of the valid data region.
            let copy_size = min(self.start - write_start, buf_size);
            self.buf[write_start..write_start+copy_size].copy_from_slice(&buf[..copy_size]);
            self.len += copy_size;
            copy_size
        } else {
            // Data in the ring does not wrap, so we're writing to the end of the buffer.
            // If there's more data to write than the space at the end we'll have to
            // do a second write to the start after.
            let copy_size = min(ringbuf_size - write_start, buf_size);
            self.buf[write_start..write_start+copy_size].copy_from_slice(&buf[..copy_size]);
            self.len += copy_size;

            // If didn't copy the entire source buffer, copy as much of the remainder as
            // possible to the start of the ring buffer.
            if copy_size < buf_size {
                copy_size + self.read_from(&buf[copy_size..])
            } else {
                copy_size
            }
        }
    }

    pub fn write_to(&mut self, buf: &mut [u8]) -> usize {
        use std::cmp::min;

        let ringbuf_size = self.buf.len();

        // Number of bytes we might read.
        let buf_size = min(self.len, buf.len());

        if buf_size == 0 {
            0
        } else if (self.start + buf_size) % ringbuf_size <= self.start {
            // The ring buffer wraps and we're reading out over that wrap.
            // We need to read the data out in two pieces, starting with the slice to the end.
            let copy_size = ringbuf_size - self.start;
            buf[..copy_size].copy_from_slice(&self.buf[self.start..]);
            self.start = 0;
            self.len -= copy_size;

            // Finally perform a read from the start of the ring buffer
            copy_size + self.write_to(&mut buf[copy_size..])
        } else {
            // We're not performing a wrapping read, so can complete in one slice
            buf[..buf_size].copy_from_slice(&self.buf[self.start..self.start+buf_size]);
            self.start += buf_size;
            self.len -= buf_size;
            buf_size
        }
    }
}

struct CursorBufferReader {
    buffer: Arc<Mutex<CursorBuffer>>
}

struct CursorBufferWriter {
    buffer: Arc<Mutex<CursorBuffer>>
}

impl Read for CursorBufferReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut ringbuf = self.buffer.lock().unwrap();
        Ok(ringbuf.write_to(buf))
    }
}

impl Write for CursorBufferWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut ringbuf = self.buffer.lock().unwrap();
        Ok(ringbuf.read_from(buf))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn spsc_stream(size: usize) -> (impl Write, impl Read) {

    let buffer = Arc::new(Mutex::new(CursorBuffer::new(size)));

    let producer = CursorBufferWriter {buffer: buffer.clone()};
    let consumer = CursorBufferReader {buffer};

    (producer, consumer)
}
