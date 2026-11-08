use elara_os::script::{control::Control, pipe};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{assert, assert_eq, vec};

#[test]
fn bounded_pipe_backpressure_is_real_and_binary_bytes_round_trip() {
    let control = Control::new();
    let (mut reader, mut writer, probe) = pipe::bounded(
        pipe::Config {
            chunk_bytes: 13,
            queued_chunks: 2,
        },
        control.worker().unwrap(),
        control.worker().unwrap(),
    )
    .unwrap();
    let payload: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();
    let done = Arc::new(AtomicBool::new(false));
    std::thread::scope(|scope| {
        let sent = payload.clone();
        let completed = done.clone();
        let handle = scope.spawn(move || {
            writer.write_all(&sent).unwrap();
            writer.flush().unwrap();
            completed.store(true, Ordering::Release);
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while probe.stats().unwrap().blocked_writes == 0
            && !done.load(Ordering::Acquire)
            && Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        let blocked = probe.stats().unwrap().blocked_writes > 0;
        if !blocked || done.load(Ordering::Acquire) {
            control.cancel().unwrap();
        }
        assert!(blocked, "writer must really block before the reader starts");
        assert!(!done.load(Ordering::Acquire), "no unbounded producer queue");
        let mut received = Vec::new();
        reader.read_to_end(&mut received).unwrap();
        handle.join().unwrap();
        assert_eq!(received, payload);
    });
    let stats = probe.stats().unwrap();
    assert_eq!(stats.enqueued, 10000);
    assert_eq!(stats.read, 10000);
    assert!(stats.peak_queued_bytes <= 26);
}
#[test]
fn closed_reader_wakes_a_backpressured_writer_without_hanging() {
    let control = Control::new();
    let (reader, mut writer, probe) = pipe::bounded(
        pipe::Config {
            chunk_bytes: 8,
            queued_chunks: 1,
        },
        control.worker().unwrap(),
        control.worker().unwrap(),
    )
    .unwrap();
    std::thread::scope(|scope| {
        let handle = scope.spawn(move || {
            writer
                .write_all(&vec![b'x'; 10000])
                .and_then(|_| writer.flush())
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while probe.stats().unwrap().blocked_writes == 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let blocked = probe.stats().unwrap().blocked_writes > 0;
        if !blocked {
            control.cancel().unwrap();
        }
        drop(reader);
        assert!(blocked);
        assert_eq!(
            handle.join().unwrap().unwrap_err().kind(),
            std::io::ErrorKind::BrokenPipe
        );
    });
}
#[test]
fn cancelled_empty_input_wakes_a_reader_instead_of_looking_like_eof() {
    let control = Control::new();
    let (mut reader, writer, _) = pipe::bounded(
        pipe::Config::default(),
        control.worker().unwrap(),
        control.worker().unwrap(),
    )
    .unwrap();
    std::thread::scope(|scope| {
        let handle = scope.spawn(move || reader.read(&mut [0; 8]));
        control.cancel().unwrap();
        assert_eq!(
            handle.join().unwrap().unwrap_err().kind(),
            std::io::ErrorKind::ConnectionAborted
        );
        drop(writer);
    });
}
#[test]
fn writer_drop_is_real_eof_and_does_not_flush_or_retry_pending_bytes() {
    let control = Control::new();
    let (mut reader, mut writer, probe) = pipe::bounded(
        pipe::Config::default(),
        control.worker().unwrap(),
        control.worker().unwrap(),
    )
    .unwrap();
    writer.write_all(b"unpublished").unwrap();
    drop(writer);
    assert_eq!(reader.read(&mut [0; 32]).unwrap(), 0);
    assert_eq!(probe.stats().unwrap().enqueued, 0);
}
#[test]
fn pause_is_only_stopped_after_worker_ack_and_resume_releases_it() {
    let control = Control::new();
    let worker = control.worker().unwrap();
    control.request_pause().unwrap();
    assert!(!control.wait_stopped(Duration::ZERO).unwrap());
    std::thread::scope(|scope| {
        let handle = scope.spawn(move || worker.checkpoint());
        assert!(control.wait_stopped(Duration::from_secs(2)).unwrap());
        let snapshot = control.snapshot().unwrap();
        assert_eq!(snapshot.workers, 1);
        assert_eq!(snapshot.stopped_workers, 1);
        control.resume().unwrap();
        handle.join().unwrap().unwrap();
    });
    assert_eq!(control.snapshot().unwrap().workers, 0);
}
#[test]
fn cancellation_wakes_a_paused_worker_and_a_long_timer() {
    let control = Control::new();
    let worker = control.worker().unwrap();
    control.request_pause().unwrap();
    std::thread::scope(|scope| {
        let handle = scope.spawn(move || worker.sleep(Duration::from_secs(10)));
        assert!(control.wait_stopped(Duration::from_secs(2)).unwrap());
        let before = Instant::now();
        control.cancel().unwrap();
        assert_eq!(
            handle.join().unwrap().unwrap_err().kind(),
            std::io::ErrorKind::ConnectionAborted
        );
        assert!(before.elapsed() < Duration::from_secs(2));
    });
}
#[test]
fn write_all_cancellation_is_nonretryable_even_while_backpressured() {
    let control = Control::new();
    let (reader, mut writer, probe) = pipe::bounded(
        pipe::Config {
            chunk_bytes: 8,
            queued_chunks: 1,
        },
        control.worker().unwrap(),
        control.worker().unwrap(),
    )
    .unwrap();
    std::thread::scope(|scope| {
        let handle = scope.spawn(move || writer.write_all(&vec![b'x'; 10000]));
        let deadline = Instant::now() + Duration::from_secs(2);
        while probe.stats().unwrap().blocked_writes == 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let blocked = probe.stats().unwrap().blocked_writes > 0;
        control.cancel().unwrap();
        drop(reader);
        let before = Instant::now();
        assert_eq!(
            handle.join().unwrap().unwrap_err().kind(),
            std::io::ErrorKind::ConnectionAborted
        );
        assert!(blocked);
        assert!(before.elapsed() < Duration::from_secs(2));
    });
}

#[test]
fn invalid_pipe_budgets_are_rejected_before_endpoint_allocation() {
    let control = Control::new();
    for config in [
        pipe::Config {
            chunk_bytes: 0,
            queued_chunks: 1,
        },
        pipe::Config {
            chunk_bytes: 4097,
            queued_chunks: 1,
        },
        pipe::Config {
            chunk_bytes: 4,
            queued_chunks: 9,
        },
    ] {
        assert!(
            pipe::bounded(config, control.worker().unwrap(), control.worker().unwrap()).is_err()
        );
    }
    assert_eq!(control.snapshot().unwrap().workers, 0);
}
