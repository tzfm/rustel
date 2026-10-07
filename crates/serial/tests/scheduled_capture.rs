use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use rustel_serial::{SerialPort, SerialSender};

struct HeldPort {
    entered: Sender<()>,
    release: Option<Receiver<()>>,
    captured: Sender<(Instant, Instant, Vec<u8>)>,
}

impl SerialPort for HeldPort {
    fn write(&mut self, _bytes: &[u8]) -> Result<(), String> {
        Err("the sender omitted the queue deadline".into())
    }

    fn write_scheduled(&mut self, due: Instant, bytes: &[u8]) -> Result<(), String> {
        if let Some(release) = self.release.take() {
            self.entered.send(()).map_err(|error| error.to_string())?;
            release
                .recv_timeout(Duration::from_secs(5))
                .map_err(|error| error.to_string())?;
        }
        self.captured
            .send((due, Instant::now(), bytes.to_vec()))
            .map_err(|error| error.to_string())
    }
}

#[test]
fn a_delayed_write_retains_its_queue_deadline() {
    let (entered, waiting) = mpsc::channel();
    let (release, held) = mpsc::channel();
    let (captured, messages) = mpsc::channel();
    let sender = SerialSender::try_with_port(
        Box::new(HeldPort {
            entered,
            release: Some(held),
            captured,
        }),
        "capture".into(),
    )
    .expect("sender");
    let first_due = Instant::now();
    assert!(sender.send_at(first_due, vec![1]));
    waiting
        .recv_timeout(Duration::from_secs(5))
        .expect("first write entered the port");
    let second_due = Instant::now();
    assert!(sender.send_at(second_due, vec![2]));
    let released = Instant::now();
    release.send(()).expect("release the port");

    for (due, bytes) in [(first_due, vec![1]), (second_due, vec![2])] {
        let (captured_due, received, captured_bytes) = messages
            .recv_timeout(Duration::from_secs(5))
            .expect("scheduled write reached the port");
        assert_eq!(captured_due, due);
        assert!(received >= released);
        assert_eq!(captured_bytes, bytes);
    }
    sender.close();
}
