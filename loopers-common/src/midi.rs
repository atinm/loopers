use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use std::io;
use std::io::{ErrorKind, Write};
use arrayvec::ArrayVec;
use std::borrow::Cow;
use crate::api::{QuantizationMode};
use crate::gui_channel::{EngineState, EngineMode};

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum EngineSnapshotField {
    Unknown, // wut?
    EngineState,
    EngineMode,
    QuantizationMode,
    Metronome,
    ActiveLooper,
    Part,
    LooperCount,
    LooperMode,
}

impl EngineSnapshotField {
    pub fn from_str(
        field: &str,
    ) -> Result<EngineSnapshotField, String> {
        Ok(match field {
            "EngineState" => EngineSnapshotField::EngineState,
            "EngineMode" => EngineSnapshotField::EngineMode,
            "QuantizationMode" => EngineSnapshotField::QuantizationMode,
            "Metronome" => EngineSnapshotField::Metronome,
            "ActiveLooper" => EngineSnapshotField::ActiveLooper,
            "Part" => EngineSnapshotField::Part,
            "LooperCount" => EngineSnapshotField::LooperCount,
            "LooperMode" => EngineSnapshotField::LooperMode,
            _ => EngineSnapshotField::Unknown,
        })
    }
}

#[derive(Debug)]
pub enum MidiEvent {
    ControllerChange {
        channel: u8,
        controller: u8,
        data: u8,
    },
}

impl MidiEvent {
    pub fn from_bytes(bs: &[u8]) -> Option<Self> {
        if bs.len() == 3 && bs[0] >> 4 == 0xb {
            Some(MidiEvent::ControllerChange {
                channel: bs[0] & 0b1111,
                controller: bs[1],
                data: bs[2],
            })
        } else {
            None
        }
    }
    pub fn to_bytes(me: &MidiEvent) -> [u8;3] {
        match me {
            MidiEvent::ControllerChange { channel, controller, data } => {
                [
                    (0xB0 /* ControllerChange Command */ | (((*channel) - 1) & 0x0F)) as u8, /* channel */
                    *controller, /* controller */
                    *data, /* data */
                ]
            }
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct MidiEngineStateSnapshot {
    pub engine_state: EngineState,
    pub engine_mode: EngineMode,
    pub sync_mode: QuantizationMode,
    pub metronome: u8,
    pub active_looper: u8,
    pub part: u8,
    pub looper_count: u8,
}

#[derive(Clone, Debug)]
pub enum MidiOutCommand {
    StateSnapshot(MidiEngineStateSnapshot),
}


#[derive(Clone)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Clone)]
pub struct LogMessage {
    buffer: ArrayVec<u8, 256>,
    len: usize,
    #[allow(unused)]
    level: LogLevel,
}

impl LogMessage {
    pub fn new() -> Self {
        LogMessage {
            buffer: ArrayVec::new(),
            len: 0,
            level: LogLevel::Info,
        }
    }

    pub fn error() -> Self {
        LogMessage {
            buffer: ArrayVec::new(),
            len: 0,
            level: LogLevel::Error,
        }
    }

    pub fn as_str(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.buffer[0..self.len])
    }
}

impl Write for LogMessage {
    fn write(&mut self, s: &[u8]) -> io::Result<usize> {
        self.len += s.len();
        self.buffer.write(s)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct MidiOutSender {
    cmd_channel: Option<Sender<MidiOutCommand>>,
    cur_message: LogMessage,
    log_channel: Option<Sender<LogMessage>>,
}

pub struct MidiOutReceiver {
    pub cmd_channel: Receiver<MidiOutCommand>,
    pub log_channel: Receiver<LogMessage>,
}
impl MidiOutSender {
    pub fn new() -> (MidiOutSender, MidiOutReceiver) {
        let (tx, rx) = bounded(100);
        let (log_tx, log_rx) = bounded(10);

        let sender = MidiOutSender {
            cmd_channel: Some(tx),
            cur_message: LogMessage::new(),
            log_channel: Some(log_tx),
        };

        let receiver = MidiOutReceiver {
            cmd_channel: rx,
            log_channel: log_rx,
        };

        (sender, receiver)
    }

    pub fn disconnected() -> MidiOutSender {
        MidiOutSender {
            cmd_channel: None,
            cur_message: LogMessage::new(),
            log_channel: None,
        }
    }

    pub fn send_update(&mut self, cmd: MidiOutCommand) {
        if let Some(midi_out_sender) = &self.cmd_channel {
            match midi_out_sender.try_send(cmd) {
                Ok(_) => {}
                Err(TrySendError::Full(_)) => {
                    warn!("MIDI Out message queue is full");
                }
                Err(TrySendError::Disconnected(_)) => {
                    // we're shutting down
                }
            }
        }
    }

    pub fn send_log(&mut self, message: LogMessage) -> () {
        if let Err(e) = self.send_log_with_result(message) {
            warn!("Failed to send midi out message: {}", e);
        }
    }

    fn send_log_with_result(&mut self, message: LogMessage) -> io::Result<()> {
        if let Some(log_channel) = &self.log_channel {
            log_channel.try_send(message).map_err(|e| match e {
                TrySendError::Full(_) => io::Error::new(ErrorKind::WouldBlock, "queue full"),
                TrySendError::Disconnected(_) => {
                    io::Error::new(ErrorKind::BrokenPipe, "queue disconnected")
                }
            })?;
        }
        Ok(())
    }
}

impl Clone for MidiOutSender {
    fn clone(&self) -> Self {
        MidiOutSender {
            cmd_channel: self.cmd_channel.clone(),
            cur_message: self.cur_message.clone(),
            log_channel: self.log_channel.clone(),
        }
    }
}

impl Write for MidiOutSender {
    fn write(&mut self, s: &[u8]) -> io::Result<usize> {
        self.cur_message.len += s.len();
        self.cur_message.buffer.write(s)
    }

    fn flush(&mut self) -> io::Result<()> {
        let message = self.cur_message.clone();
        self.cur_message.len = 0;
        self.cur_message.buffer.clear();
        self.send_log_with_result(message)
    }
}
