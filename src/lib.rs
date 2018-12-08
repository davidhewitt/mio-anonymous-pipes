extern crate mio;
extern crate miow;
extern crate winapi;

use mio::{Evented, Poll, PollOpt, Ready, Registration, SetReadiness, Token};
use miow::pipe::{AnonRead, AnonWrite};

use winapi::um::ioapiset::CancelSynchronousIo;

use std::io;
use std::os::windows::io::AsRawHandle;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}, Condvar, Mutex};
use std::thread::{JoinHandle, spawn};

extern crate spsc_buffer;
use spsc_buffer::*;

struct WaitTag {}

struct EventedAnonReadInner {
    registration: Registration,
    readiness: SetReadiness,
    done: AtomicBool,
    sig_buffer_not_full: Condvar,
    wait_tag: Mutex<WaitTag>
}

pub struct EventedAnonRead {
    // Is an Option so it can be moved out and joined in the Drop impl.
    thread: Option<JoinHandle<()>>,
    consumer: SpscBufferReader,
    inner: Arc<EventedAnonReadInner>
}

impl EventedAnonRead {
    pub fn new(mut pipe: AnonRead) -> Self {
        let (registration, readiness) = Registration::new2();

        let (mut producer, consumer) = spsc_buffer(65536);

        let done = AtomicBool::new(false);

        let sig_buffer_not_full = Condvar::new();
        let wait_tag = Mutex::new(WaitTag {});

        let inner = Arc::new(
            EventedAnonReadInner { registration, readiness, done, sig_buffer_not_full, wait_tag }
        );

        let thread = {
            let inner = inner.clone();
            spawn(move || {
                use std::io::Read;

                let mut tmp_buf = [0u8; 65535];

                loop {
                    if inner.done.load(Ordering::SeqCst) { break; }

                    // Read into temp buffer before we grab the lock.
                    let result = pipe.read(&mut tmp_buf[..]);

                    if let Ok(nbytes) = &result {
                        let mut written = 0usize;

                        while written < *nbytes {
                            // Wait for buffer to clear if need be.
                            if producer.is_full() {
                                let wait_tag = inner.wait_tag.lock().unwrap();
                                let _ = inner.sig_buffer_not_full.wait(wait_tag).unwrap();
                            }

                            written += producer.write_from_slice(&tmp_buf[written..*nbytes]);

                            if !inner.readiness.readiness().is_readable() {
                                inner.readiness.set_readiness(Ready::readable()).unwrap();
                            }
                        }
                    }
                }
            })
        };

        Self { thread: Some(thread), consumer, inner }
    }
}

impl io::Read for EventedAnonRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let was_full = self.consumer.is_full();

        let nbytes = self.consumer.read_to_slice(buf);

        if nbytes > 0 && was_full {
            self.inner.sig_buffer_not_full.notify_one();
        }

        if self.consumer.is_empty() {
            self.inner.readiness.set_readiness(Ready::empty()).unwrap();

            // Possible race: the consumer may think the queue is empty but by the time
            // the readiness is set the producer thread may have written data
            //
            // We avoid the race by re-checking the queue is empty like this, and undo the
            // readiness setting if necessary.
            if !self.consumer.is_empty() {
                self.inner.readiness.set_readiness(Ready::empty()).unwrap();
            }
        }

        Ok(nbytes)
    }
}

impl Evented for EventedAnonRead {
    fn register(&self,
                poll: &Poll,
                token: Token,
                interest: Ready,
                opts: PollOpt) -> io::Result<()> {
        poll.register(&self.inner.registration, token, interest, opts)
    }

    fn reregister(&self,
                poll: &Poll,
                token: Token,
                interest: Ready,
                opts: PollOpt) -> io::Result<()> {
        poll.reregister(&self.inner.registration, token, interest, opts)
    }

    fn deregister(&self, poll: &Poll) -> io::Result<()> {
        poll.deregister(&self.inner.registration)
    }
}

impl Drop for EventedAnonRead {
    fn drop(&mut self) {
        self.inner.done.store(true, Ordering::SeqCst);

        let thread = self.thread.take().unwrap();

        // Stop reader thread waiting for pipe contents
        unsafe { CancelSynchronousIo(thread.as_raw_handle()); }

        thread.join().expect("Could not close EventedAnonRead worker");
    }
}

struct EventedAnonWriteInner {
    registration: Registration,
    readiness: SetReadiness,
    done: AtomicBool,
    sig_buffer_not_empty: Condvar,
    wait_tag: Mutex<WaitTag>
}

pub struct EventedAnonWrite {
    // Is an Option so it can be moved out and joined in the Drop impl
    thread: Option<JoinHandle<()>>,
    producer: SpscBufferWriter,
    inner: Arc<EventedAnonWriteInner>
}

impl EventedAnonWrite {
    pub fn new(mut pipe: AnonWrite) -> Self {
        let (registration, readiness) = Registration::new2();

        let (producer, mut consumer) = spsc_buffer(65536);

        let done = AtomicBool::new(false);

        let sig_buffer_not_empty = Condvar::new();
        let wait_tag = Mutex::new(WaitTag {});

        let inner = Arc::new(
            EventedAnonWriteInner { registration, readiness, done, sig_buffer_not_empty, wait_tag }
        );

        let thread = {
            let inner = inner.clone();
            spawn(move || {
                use std::io::Write;
                let mut tmp_buf = [0u8; 65535];

                inner.readiness.set_readiness(Ready::writable()).unwrap();

                loop {
                    if inner.done.load(Ordering::SeqCst) {
                        break;
                    }

                    // Read into temp buffer while holding the lock
                    let nbytes = {
                        // Wait for buffer to have contents
                        if consumer.is_empty() {
                            let wait_tag = inner.wait_tag.lock().unwrap();
                            let _ = inner.sig_buffer_not_empty.wait(wait_tag).unwrap();
                        }

                        let nbytes = consumer.read_to_slice(&mut tmp_buf);

                        if !inner.readiness.readiness().is_writable() {
                            inner.readiness.set_readiness(Ready::writable()).unwrap();
                        }

                        nbytes
                    };

                    let mut written = 0usize;
                    while written < nbytes {
                        written += pipe.write(&tmp_buf[written..nbytes]).unwrap();
                    }
                }
            })
        };

        Self { thread: Some(thread), producer, inner }
    }
}

impl io::Write for EventedAnonWrite {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let was_empty = self.producer.is_empty();

        // TODO: Handle the possible read errors.
        let nbytes = self.producer.write_from_slice(buf);

        if nbytes > 0 && was_empty {
            self.inner.sig_buffer_not_empty.notify_one();
        }

        if self.producer.is_full() {
            self.inner.readiness.set_readiness(Ready::empty()).unwrap();

            // Possible race: the producer may think the buffer is full but by the time
            // the readiness is set the consumer thread may have read data
            //
            // It is sufficient to re-check the buffer is empty, and undo the readiness
            // setting to work around this.
            if !self.producer.is_full() {
                self.inner.readiness.set_readiness(Ready::writable()).unwrap();
            }
        }

        Ok(nbytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Evented for EventedAnonWrite {
    fn register(&self,
                poll: &Poll,
                token: Token,
                interest: Ready,
                opts: PollOpt) -> io::Result<()> {
        poll.register(&self.inner.registration, token, interest, opts)
    }

    fn reregister(&self,
                poll: &Poll,
                token: Token,
                interest: Ready,
                opts: PollOpt) -> io::Result<()> {
        poll.reregister(&self.inner.registration, token, interest, opts)
    }

    fn deregister(&self, poll: &Poll) -> io::Result<()> {
        poll.deregister(&self.inner.registration)
    }
}

impl Drop for EventedAnonWrite {
    fn drop(&mut self) {
        self.inner.done.store(true, Ordering::SeqCst);

        // Stop the writer thread waiting for contents
        self.inner.sig_buffer_not_empty.notify_one();

        self.thread.take().unwrap().join().expect("Could not close EventedAnonWrite worker");
    }
}
