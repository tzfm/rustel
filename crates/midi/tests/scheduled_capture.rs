use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use rustel_midi::{MidiMessage, MidiPort, MidiSender};

struct HeldPort {
    entered: Sender<()>,
    release: Option<Receiver<()>>,
    captured: Sender<(Instant, Instant, Vec<u8>)>,
}

impl MidiPort for HeldPort {
    fn send(&mut self, _bytes: &[u8]) -> Result<(), String> {
        Err("the sender omitted the queue deadline".into())
    }

    fn send_scheduled(&mut self, due: Instant, bytes: &[u8]) -> Result<(), String> {
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
fn a_delayed_send_retains_its_queue_deadline() {
    let (entered, waiting) = mpsc::channel();
    let (release, held) = mpsc::channel();
    let (captured, messages) = mpsc::channel();
    let sender = MidiSender::try_with_port(
        Box::new(HeldPort {
            entered,
            release: Some(held),
            captured,
        }),
        "capture".into(),
    )
    .expect("sender");
    let first_due = Instant::now();
    assert!(sender.send_at(first_due, MidiMessage::note_on(1, 60, 100)));
    waiting
        .recv_timeout(Duration::from_secs(5))
        .expect("first send entered the port");
    let second_due = Instant::now();
    assert!(sender.send_at(second_due, MidiMessage::note_on(1, 62, 100)));
    let released = Instant::now();
    release.send(()).expect("release the port");

    for (due, note) in [(first_due, 60), (second_due, 62)] {
        let (captured_due, received, bytes) = messages
            .recv_timeout(Duration::from_secs(5))
            .expect("scheduled send reached the port");
        assert_eq!(captured_due, due);
        assert!(received >= released);
        assert_eq!(bytes, vec![0x90, note, 100]);
    }
}
