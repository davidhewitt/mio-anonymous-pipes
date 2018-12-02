#[macro_use]
extern crate criterion;
extern crate spsc_stream;

use criterion::Criterion;

use std::io::{Read, Write};
use std::thread;

fn criterion_benchmark(c: &mut Criterion) {

    const BUFSIZE: usize = 1000;

    c.bench_function(
        "write 1000 bytes",
        |b| b.iter(|| {
            let (mut producer, mut consumer) = spsc_stream::spsc_stream(100);

            let handle = thread::spawn(move || {
                let buf = [1u8; BUFSIZE];

                let mut written = 0;
                while written < BUFSIZE {
                    written += producer.write(&buf[written..])
                        .expect("failed to write");
                }
            });

            let mut buf = [0u8; BUFSIZE];

            let mut written = 0;
            while written < BUFSIZE {
                written += consumer.read(&mut buf[written..])
                    .expect("failed to read");
            }

            handle.join().expect("could not join thread");

            assert_eq!(&buf[..], &[1u8; BUFSIZE][..]);

        })
    );
}

criterion_group!(
    name = benches;
    config = criterion::Criterion::default().sample_size(5);
    targets = criterion_benchmark
);
criterion_main!(benches);
