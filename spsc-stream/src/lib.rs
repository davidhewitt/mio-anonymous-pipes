use std::cell::UnsafeCell;
use std::io::{self, Read, Write};
use std::mem;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct RingBuffer {
    buf: UnsafeCell<Box<[u8]>>,
    len: AtomicUsize,
}

impl RingBuffer {
    fn new(size: usize) -> Self {
        Self {
            buf: UnsafeCell::new(vec![0; size].into_boxed_slice()),
            len: AtomicUsize::new(0)
        }
    }
}

pub struct RingBufferReader {
    start: usize,
    buffer: Arc<RingBuffer>
}

impl RingBufferReader {
    pub fn is_empty(&self) -> bool {
        self.buffer.len.load(Ordering::SeqCst) == 0
    }

    pub fn is_full(&self) -> bool {
        self.buffer.len.load(Ordering::SeqCst) == unsafe {&*self.buffer.buf.get()}.len()
    }
}

impl Read for RingBufferReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use std::cmp::min;

        let ringbuf: &mut Box<[u8]> = unsafe {mem::transmute(self.buffer.buf.get())};

        let ringbuf_capacity = ringbuf.len();
        let ringbuf_len = self.buffer.len.load(Ordering::SeqCst);

        // Max number of bytes we might read
        let read_size = min(buf.len(), ringbuf_len);

        buf[..read_size].copy_from_slice(&ringbuf[self.start..self.start+read_size]);
        self.start = (self.start + read_size) % ringbuf_capacity;
        self.buffer.len.fetch_sub(read_size, Ordering::SeqCst);

        Ok(read_size)
    }
}

unsafe impl Sync for RingBufferReader {}
unsafe impl Send for RingBufferReader {}

pub struct RingBufferWriter {
    end: usize,
    buffer: Arc<RingBuffer>
}

impl RingBufferWriter {
    pub fn is_empty(&self) -> bool {
        self.buffer.len.load(Ordering::SeqCst) == 0
    }

    pub fn is_full(&self) -> bool {
        self.buffer.len.load(Ordering::SeqCst) == unsafe {&*self.buffer.buf.get()}.len()
    }
}

unsafe impl Sync for RingBufferWriter {}
unsafe impl Send for RingBufferWriter {}

impl Write for RingBufferWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        use std::cmp::min;

        let ringbuf: &mut Box<[u8]> = unsafe {mem::transmute(self.buffer.buf.get())};

        let ringbuf_capacity = ringbuf.len();
        let ringbuf_len = self.buffer.len.load(Ordering::SeqCst);

        // Max number of bytes we might read
        let max_write_size = min(buf.len(), ringbuf_capacity - ringbuf_len);
        let space_until_end = ringbuf_capacity - self.end;
        let write_size = min(max_write_size, space_until_end);

        ringbuf[self.end..self.end+write_size].copy_from_slice(&buf[..write_size]);
        self.end = (self.end + write_size) % ringbuf_capacity;
        self.buffer.len.fetch_add(write_size, Ordering::SeqCst);

        Ok(write_size)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn spsc_stream(size: usize) -> (RingBufferWriter, RingBufferReader) {

    let buffer = Arc::new(RingBuffer::new(size));

    let producer = RingBufferWriter {end: 0, buffer: buffer.clone()};
    let consumer = RingBufferReader {start: 0, buffer};

    (producer, consumer)
}
