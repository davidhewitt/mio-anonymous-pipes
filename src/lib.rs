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

extern crate spsc_stream;
use spsc_stream::*;

struct AsyncAnonReadInner {
    registration: Registration,
    readiness: SetReadiness,
    buffer: Mutex<CursorBuffer>,
    done: AtomicBool,
    sig_buffer_not_full: Condvar
}

pub struct AsyncAnonRead {
    // Is an Option so it can be moved out and joined in the Drop impl.
    thread: Option<JoinHandle<()>>,
    inner: Arc<AsyncAnonReadInner>
}

impl AsyncAnonRead {
    pub fn new(mut pipe: AnonRead) -> Self {
        let (registration, readiness) = Registration::new2();

        let buffer = Mutex::new(CursorBuffer::new(65536));

        let done = AtomicBool::new(false);

        let sig_buffer_not_full = Condvar::new();

        let inner = Arc::new(
            AsyncAnonReadInner { registration, readiness, buffer, done, sig_buffer_not_full }
        );

        let thread = {
            let inner = inner.clone();
            spawn(move || {
                use std::io::Read;

                let mut tmp_buf = [0u8; 65535];

                loop {
                    if inner.done.load(Ordering::SeqCst) { break; }

                    {
                        // Read into temp buffer before we grab the lock.
                        let result = pipe.read(&mut tmp_buf[..]);

                        if let Ok(nbytes) = &result {
                            let mut written = 0usize;
                            let mut buf = inner.buffer.lock().unwrap();

                            while written < *nbytes {
                                // Wait for buffer to clear if need be.
                                if buf.is_full() {
                                    buf = inner.sig_buffer_not_full.wait(buf).unwrap();
                                }

                                let was_empty = buf.is_empty();

                                written += buf.read_from(&tmp_buf[written..*nbytes]);

                                if was_empty {
                                    inner.readiness.set_readiness(Ready::readable()).unwrap();
                                }
                            }
                        }
                    }
                }
            })
        };

        Self { thread: Some(thread), inner }
    }
}

impl io::Read for AsyncAnonRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use std::sync::TryLockError;

        // TODO: Handle the possible read errors.
        match self.inner.buffer.try_lock() {
            Ok(mut inner_buf) => {
                let was_full = inner_buf.is_full();
                let nbytes = inner_buf.write_to(buf);
                if inner_buf.is_empty() {
                    self.inner.readiness.set_readiness(Ready::empty()).unwrap();
                }
                if was_full {
                    self.inner.sig_buffer_not_full.notify_one();
                }
                Ok(nbytes)
            },
            Err(TryLockError::WouldBlock) => Ok(0), // There's still more to read, we'll try again later.
            Err(TryLockError::Poisoned(_)) => panic!("Poisoned lock")
        }
    }
}

impl Evented for AsyncAnonRead {
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

impl Drop for AsyncAnonRead {
    fn drop(&mut self) {
        self.inner.done.store(true, Ordering::SeqCst);

        let thread = self.thread.take().unwrap();

        // Stop reader thread waiting for pipe contents
        unsafe { CancelSynchronousIo(thread.as_raw_handle()); }

        thread.join().expect("Could not close AsyncAnonRead worker");
    }
}

struct AsyncAnonWriteInner {
    registration: Registration,
    readiness: SetReadiness,
    buffer: Mutex<CursorBuffer>,
    done: AtomicBool,
    sig_buffer_not_empty: Condvar
}

pub struct AsyncAnonWrite {
    // Is an Option so it can be moved out and joined in the Drop impl
    thread: Option<JoinHandle<()>>,
    inner: Arc<AsyncAnonWriteInner>
}

impl AsyncAnonWrite {
    pub fn new(mut pipe: AnonWrite) -> Self {
        let (registration, readiness) = Registration::new2();

        let buffer = Mutex::new(CursorBuffer::new(65536));

        let done = AtomicBool::new(false);

        let sig_buffer_not_empty = Condvar::new();

        let inner = Arc::new(
            AsyncAnonWriteInner { registration, readiness, buffer, done, sig_buffer_not_empty }
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
                        let mut buf = inner.buffer.lock().unwrap();

                        // Wait for buffer to have contents
                        if buf.is_empty() {
                            buf = inner.sig_buffer_not_empty.wait(buf).unwrap();
                        }

                        let was_full = buf.is_full();

                        let nbytes = buf.write_to(&mut tmp_buf);

                        if was_full {
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

        Self { thread: Some(thread), inner }
    }
}

impl io::Write for AsyncAnonWrite {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        use std::sync::TryLockError;

        // TODO: Handle the possible read errors.
        match self.inner.buffer.try_lock() {
            Ok(mut inner_buf) => {
                let was_empty = inner_buf.is_empty();
                let nbytes = inner_buf.read_from(buf);
                if inner_buf.is_full() {
                    self.inner.readiness.set_readiness(Ready::empty()).unwrap();
                }
                if was_empty {
                    self.inner.sig_buffer_not_empty.notify_one();
                }
                Ok(nbytes)
            },
            Err(TryLockError::WouldBlock) => Ok(0), // There's still more to read, we'll try again later.
            Err(TryLockError::Poisoned(_)) => panic!("Poisoned lock")
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Evented for AsyncAnonWrite {
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

impl Drop for AsyncAnonWrite {
    fn drop(&mut self) {
        self.inner.done.store(true, Ordering::SeqCst);

        // Stop the writer thread waiting for contents
        self.inner.sig_buffer_not_empty.notify_one();

        self.thread.take().unwrap().join().expect("Could not close AsyncAnonWrite worker");
    }
}
