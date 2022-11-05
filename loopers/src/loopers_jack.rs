use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError};
use jack::{AudioOut, MidiWriter, Port, ProcessScope};
use loopers_common::api::{Command, LooperMode, QuantizationMode};
use loopers_common::config::MidiOutMapping;
use loopers_common::gui_channel::{EngineMode, EngineState, GuiSender};
use loopers_common::midi::MidiEvent;
use loopers_common::midi::{
    EngineSnapshotField, MidiEngineStateSnapshot, MidiOutCommand, MidiOutReceiver, MidiOutSender,
};
use loopers_common::Host;
use loopers_engine::Engine;
use loopers_gui::Gui;
use std::collections::HashMap;
use std::{io, thread};

enum ClientChange {
    AddPort(u32),
    RemovePort(u32, Port<AudioOut>, Port<AudioOut>),
    Shutdown,
}

enum ClientChangeResponse {
    PortAdded(u32, Port<AudioOut>, Port<AudioOut>),
}

pub struct JackHost<'a> {
    looper_ports: &'a mut HashMap<u32, [Port<AudioOut>; 2]>,
    ps: Option<&'a ProcessScope>,
    port_change_tx: Sender<ClientChange>,
    port_change_resp: Receiver<ClientChangeResponse>,
}

impl<'a> Host<'a> for JackHost<'a> {
    fn add_looper(&mut self, id: u32) -> Result<(), String> {
        if !self.looper_ports.contains_key(&id) {
            if let Err(e) = self.port_change_tx.try_send(ClientChange::AddPort(id)) {
                warn!("Failed to send port add request: {:?}", e);
            }
        }

        Ok(())
    }

    fn remove_looper(&mut self, id: u32) -> Result<(), String> {
        if let Some([l, r]) = self.looper_ports.remove(&id) {
            if let Err(e) = self
                .port_change_tx
                .try_send(ClientChange::RemovePort(id, l, r))
            {
                warn!("Failed to send port remove request: {:?}", e);
            }
        }

        Ok(())
    }

    fn output_for_looper<'b>(&'b mut self, id: u32) -> Option<[&'b mut [f32]; 2]>
    where
        'a: 'b,
    {
        match self.port_change_resp.try_recv() {
            Ok(ClientChangeResponse::PortAdded(id, l, r)) => {
                self.looper_ports.insert(id, [l, r]);
            }
            Err(_) => {}
        }

        let ps = self.ps?;
        let [l, r] = self.looper_ports.get_mut(&id)?;
        Some([l.as_mut_slice(ps), r.as_mut_slice(ps)])
    }
}

struct Notifications;

impl jack::NotificationHandler for Notifications {
    fn thread_init(&self, _: &jack::Client) {
        debug!("JACK: thread init");
    }

    fn shutdown(&mut self, status: jack::ClientStatus, reason: &str) {
        debug!(
            "JACK: shutdown with status {:?} because \"{}\"",
            status, reason
        );
    }

    fn freewheel(&mut self, _: &jack::Client, is_enabled: bool) {
        debug!(
            "JACK: freewheel mode is {}",
            if is_enabled { "on" } else { "off" }
        );
    }

    fn sample_rate(&mut self, _: &jack::Client, _: jack::Frames) -> jack::Control {
        jack::Control::Quit
    }

    fn client_registration(&mut self, _: &jack::Client, name: &str, is_reg: bool) {
        info!(
            "JACK: {} client with name \"{}\"",
            if is_reg { "registered" } else { "unregistered" },
            name
        );
    }

    fn port_registration(&mut self, _: &jack::Client, port_id: jack::PortId, is_reg: bool) {
        info!(
            "JACK: {} port with id {}",
            if is_reg { "registered" } else { "unregistered" },
            port_id
        );
    }

    fn port_rename(
        &mut self,
        _: &jack::Client,
        port_id: jack::PortId,
        old_name: &str,
        new_name: &str,
    ) -> jack::Control {
        info!(
            "JACK: port with id {} renamed from {} to {}",
            port_id, old_name, new_name
        );
        jack::Control::Continue
    }

    fn ports_connected(
        &mut self,
        _: &jack::Client,
        port_id_a: jack::PortId,
        port_id_b: jack::PortId,
        are_connected: bool,
    ) {
        debug!(
            "JACK: ports with id {} and {} are {}",
            port_id_a,
            port_id_b,
            if are_connected {
                "connected"
            } else {
                "disconnected"
            }
        );
    }

    fn graph_reorder(&mut self, _: &jack::Client) -> jack::Control {
        info!("JACK: graph reordered");
        jack::Control::Continue
    }

    fn xrun(&mut self, _: &jack::Client) -> jack::Control {
        warn!("JACK: xrun occurred");
        jack::Control::Continue
    }
}

fn initialize(new_state: MidiEngineStateSnapshot, engine: &mut Engine, writer: &mut MidiWriter) {
    let midi_out_mappings: &Vec<MidiOutMapping> = (*engine).midi_out_mappings();
    for i in 0..midi_out_mappings.len() {
        let mm = &midi_out_mappings[i];
        match mm.field {
            EngineSnapshotField::EngineState => {
                writer
                    .write(&jack::RawMidi {
                        time: 0,
                        bytes: &MidiEvent::to_bytes(&MidiEvent::ControllerChange {
                            channel: mm.channel,
                            controller: mm.controller,
                            data: match new_state.engine_state {
                                EngineState::Stopped => 0,
                                EngineState::Paused => 1,
                                EngineState::Active => 2,
                            },
                        }),
                    })
                    .unwrap();
            }
            EngineSnapshotField::EngineMode => {
                writer
                    .write(&jack::RawMidi {
                        time: 0,
                        bytes: &MidiEvent::to_bytes(&MidiEvent::ControllerChange {
                            channel: mm.channel,
                            controller: mm.controller,
                            data: match new_state.engine_mode {
                                EngineMode::Record => 0,
                                EngineMode::Play => 1,
                                _ => 2, // should not happen
                            },
                        }),
                    })
                    .unwrap();
            }
            EngineSnapshotField::QuantizationMode => {
                writer
                    .write(&jack::RawMidi {
                        time: 0,
                        bytes: &MidiEvent::to_bytes(&MidiEvent::ControllerChange {
                            channel: mm.channel,
                            controller: mm.controller,
                            data: match new_state.sync_mode {
                                QuantizationMode::Free => 0,
                                QuantizationMode::Beat => 1,
                                QuantizationMode::Measure => 2,
                            },
                        }),
                    })
                    .unwrap();
            }
            EngineSnapshotField::Metronome => {
                writer
                    .write(&jack::RawMidi {
                        time: 0,
                        bytes: &MidiEvent::to_bytes(&MidiEvent::ControllerChange {
                            channel: mm.channel,
                            controller: mm.controller,
                            data: new_state.metronome,
                        }),
                    })
                    .unwrap();
            }
            EngineSnapshotField::ActiveLooper => {
                writer
                    .write(&jack::RawMidi {
                        time: 0,
                        bytes: &MidiEvent::to_bytes(&MidiEvent::ControllerChange {
                            channel: mm.channel,
                            controller: mm.controller,
                            data: new_state.active_looper,
                        }),
                    })
                    .unwrap();
            }
            EngineSnapshotField::LooperCount => {
                writer
                    .write(&jack::RawMidi {
                        time: 0,
                        bytes: &MidiEvent::to_bytes(&MidiEvent::ControllerChange {
                            channel: mm.channel,
                            controller: mm.controller,
                            data: new_state.looper_count,
                        }),
                    })
                    .unwrap();
            }
            _ => {
                // nothing to do
            }
        }
    }
}

fn update_loopers(
    new_state: MidiEngineStateSnapshot,
    current_looper_modes: &Vec<LooperMode>,
    update_all_loopers: bool,
    engine: &mut Engine,
    writer: &mut MidiWriter,
) -> bool {
    let mut update = false;
    let midi_out_mappings: &Vec<MidiOutMapping> = (*engine).midi_out_mappings();
    let mm = midi_out_mappings
        .iter()
        .find(|&m| m.field == EngineSnapshotField::LooperMode);
    match mm {
        Some(m) => {
            for idx in 0..new_state.looper_count as usize {
                let looper_mode = (*engine).looper_mode_by_index(idx);
                update = update || current_looper_modes[idx] != looper_mode;
                if update_all_loopers || update {
                    match looper_mode {
                        LooperMode::Recording => {
                            writer
                                .write(&jack::RawMidi {
                                    time: 0,
                                    bytes: &MidiEvent::to_bytes(&MidiEvent::ControllerChange {
                                        channel: m.channel,
                                        controller: m.controller + idx as u8,
                                        data: 0,
                                    }),
                                })
                                .unwrap();
                        }
                        LooperMode::Overdubbing => {
                            writer
                                .write(&jack::RawMidi {
                                    time: 0,
                                    bytes: &MidiEvent::to_bytes(&MidiEvent::ControllerChange {
                                        channel: m.channel,
                                        controller: m.controller + idx as u8,
                                        data: 1,
                                    }),
                                })
                                .unwrap();
                        }
                        LooperMode::Muted => {
                            writer
                                .write(&jack::RawMidi {
                                    time: 0,
                                    bytes: &MidiEvent::to_bytes(&MidiEvent::ControllerChange {
                                        channel: m.channel,
                                        controller: m.controller + idx as u8,
                                        data: 2,
                                    }),
                                })
                                .unwrap();
                        }
                        LooperMode::Playing => {
                            writer
                                .write(&jack::RawMidi {
                                    time: 0,
                                    bytes: &MidiEvent::to_bytes(&MidiEvent::ControllerChange {
                                        channel: m.channel,
                                        controller: m.controller + idx as u8,
                                        data: 3,
                                    }),
                                })
                                .unwrap();
                        }
                        LooperMode::Soloed => {
                            writer
                                .write(&jack::RawMidi {
                                    time: 0,
                                    bytes: &MidiEvent::to_bytes(&MidiEvent::ControllerChange {
                                        channel: m.channel,
                                        controller: m.controller + idx as u8,
                                        data: 4,
                                    }),
                                })
                                .unwrap();
                        }
                        LooperMode::Armed => {
                            writer
                                .write(&jack::RawMidi {
                                    time: 0,
                                    bytes: &MidiEvent::to_bytes(&MidiEvent::ControllerChange {
                                        channel: m.channel,
                                        controller: m.controller + idx as u8,
                                        data: 5,
                                    }),
                                })
                                .unwrap();
                        }
                    }
                }
            }
        }
        _ => {}
    }

    return update;
}

fn update(
    new_state: MidiEngineStateSnapshot,
    current_state: MidiEngineStateSnapshot,
    engine: &mut Engine,
    writer: &mut MidiWriter,
) {
    let midi_out_mappings: &Vec<MidiOutMapping> = (*engine).midi_out_mappings();
    for i in 0..midi_out_mappings.len() {
        let mm = &midi_out_mappings[i];
        match mm.field {
            EngineSnapshotField::EngineState => {
                if new_state.engine_state != current_state.engine_state {
                    writer
                        .write(&jack::RawMidi {
                            time: 0,
                            bytes: &MidiEvent::to_bytes(&MidiEvent::ControllerChange {
                                channel: mm.channel,
                                controller: mm.controller,
                                data: match new_state.engine_state {
                                    EngineState::Stopped => 0,
                                    EngineState::Paused => 1,
                                    EngineState::Active => 2,
                                },
                            }),
                        })
                        .unwrap();
                }
            }
            EngineSnapshotField::EngineMode => {
                if new_state.engine_mode != current_state.engine_mode {
                    writer
                        .write(&jack::RawMidi {
                            time: 0,
                            bytes: &MidiEvent::to_bytes(&MidiEvent::ControllerChange {
                                channel: mm.channel,
                                controller: mm.controller,
                                data: match new_state.engine_mode {
                                    EngineMode::Record => 0,
                                    EngineMode::Play => 1,
                                    _ => 2, // should not happen
                                },
                            }),
                        })
                        .unwrap();
                }
            }
            EngineSnapshotField::QuantizationMode => {
                if new_state.sync_mode != current_state.sync_mode {
                    writer
                        .write(&jack::RawMidi {
                            time: 0,
                            bytes: &MidiEvent::to_bytes(&MidiEvent::ControllerChange {
                                channel: mm.channel,
                                controller: mm.controller,
                                data: match new_state.sync_mode {
                                    QuantizationMode::Free => 0,
                                    QuantizationMode::Beat => 1,
                                    QuantizationMode::Measure => 2,
                                },
                            }),
                        })
                        .unwrap();
                }
            }
            EngineSnapshotField::Metronome => {
                if new_state.metronome != current_state.metronome {
                    writer
                        .write(&jack::RawMidi {
                            time: 0,
                            bytes: &MidiEvent::to_bytes(&MidiEvent::ControllerChange {
                                channel: mm.channel,
                                controller: mm.controller,
                                data: new_state.metronome,
                            }),
                        })
                        .unwrap();
                }
            }
            EngineSnapshotField::ActiveLooper => {
                if new_state.active_looper != current_state.active_looper {
                    writer
                        .write(&jack::RawMidi {
                            time: 0,
                            bytes: &MidiEvent::to_bytes(&MidiEvent::ControllerChange {
                                channel: mm.channel,
                                controller: mm.controller,
                                data: new_state.active_looper,
                            }),
                        })
                        .unwrap();
                }
            }
            EngineSnapshotField::LooperCount => {
                if new_state.looper_count != current_state.looper_count {
                    writer
                        .write(&jack::RawMidi {
                            time: 0,
                            bytes: &MidiEvent::to_bytes(&MidiEvent::ControllerChange {
                                channel: mm.channel,
                                controller: mm.controller,
                                data: new_state.looper_count,
                            }),
                        })
                        .unwrap();
                }
            }
            _ => {
                // nothing to do
            }
        }
    }
}

pub fn jack_main(
    gui: Option<Gui>,
    gui_sender: GuiSender,
    gui_to_engine_receiver: Receiver<Command>,
    midi_out_sender: MidiOutSender,
    midi_out_receiver: MidiOutReceiver,
    beat_normal: Vec<f32>,
    beat_emphasis: Vec<f32>,
    restore: bool,
) {
    // Create client
    let (client, _status) = jack::Client::new("loopers", jack::ClientOptions::NO_START_SERVER)
        .expect("Jack server is not running");

    // Register ports. They will be used in a callback that will be
    // called when new data is available.
    let in_a = client
        .register_port("in_l", jack::AudioIn::default())
        .unwrap();
    let in_b = client
        .register_port("in_r", jack::AudioIn::default())
        .unwrap();
    let mut out_a = client
        .register_port("main_out_l", jack::AudioOut::default())
        .unwrap();
    let mut out_b = client
        .register_port("main_out_r", jack::AudioOut::default())
        .unwrap();

    let mut met_out_a = client
        .register_port("metronome_out_l", jack::AudioOut::default())
        .unwrap();
    let mut met_out_b = client
        .register_port("metronome_out_r", jack::AudioOut::default())
        .unwrap();

    let midi_in = client
        .register_port("loopers_midi_in", jack::MidiIn::default())
        .unwrap();

    let mut midi_out = client
        .register_port("loopers_midi_out", jack::MidiOut::default())
        .unwrap();

    let mut looper_ports: HashMap<u32, [Port<AudioOut>; 2]> = HashMap::new();

    let (port_change_tx, port_change_rx) = bounded(10);
    let (port_change_resp_tx, port_change_resp_rx) = bounded(10);

    let mut host = JackHost {
        looper_ports: &mut looper_ports,
        ps: None,
        port_change_tx: port_change_tx.clone(),
        port_change_resp: port_change_resp_rx.clone(),
    };

    let mut engine = Engine::new(
        &mut host,
        gui_sender,
        midi_out_sender,
        gui_to_engine_receiver,
        beat_normal,
        beat_emphasis,
        restore,
        client.sample_rate(),
    );

    let process_port_change = port_change_tx.clone();

    let mut initialized = false;
    let mut first = true;
    let mut current_state: Option<MidiEngineStateSnapshot> = None;
    let mut current_looper_modes = Vec::<LooperMode>::with_capacity(1);
    let process_callback =
        move |_client: &jack::Client, ps: &jack::ProcessScope| -> jack::Control {
            let in_bufs = [in_a.as_slice(ps), in_b.as_slice(ps)];
            let out_l = out_a.as_mut_slice(ps);
            let out_r = out_b.as_mut_slice(ps);
            for b in &mut *out_l {
                *b = 0f32;
            }
            for b in &mut *out_r {
                *b = 0f32;
            }

            for l in looper_ports.values_mut() {
                for c in l {
                    for v in c.as_mut_slice(ps) {
                        *v = 0f32;
                    }
                }
            }

            let mut met_bufs = [met_out_a.as_mut_slice(ps), met_out_b.as_mut_slice(ps)];
            for buf in &mut met_bufs {
                for b in &mut **buf {
                    *b = 0f32
                }
            }

            let mut host = JackHost {
                looper_ports: &mut looper_ports,
                ps: Some(ps),
                port_change_tx: process_port_change.clone(),
                port_change_resp: port_change_resp_rx.clone(),
            };

            let midi_events: Vec<MidiEvent> = midi_in
                .iter(ps)
                .filter_map(|e| MidiEvent::from_bytes(e.bytes))
                .collect();

            engine.process(
                &mut host,
                in_bufs,
                out_l,
                out_r,
                met_bufs,
                ps.n_frames() as u64,
                &midi_events,
            );

            loop {
                match midi_out_receiver.cmd_channel.try_recv() {
                    Ok(MidiOutCommand::StateSnapshot(state)) => {
                        let mut writer = midi_out.writer(ps);
                        if !initialized {
                            current_state = Some(state.clone());
                            current_looper_modes.clear();
                            for idx in 0..state.looper_count as usize {
                                current_looper_modes.push(engine.looper_mode_by_index(idx));
                            }
                            initialized = true;
                        } else {
                            if first && state.engine_state == EngineState::Active {
                                // the very first time the engine is active, send all state
                                initialize(state, &mut engine, &mut writer);
                                first = false;
                            } else {
                                update(state, current_state.unwrap(), &mut engine, &mut writer);
                            }
                            if current_looper_modes.len() != state.looper_count as usize {
                                current_looper_modes.clear();
                                for idx in 0..state.looper_count as usize {
                                    current_looper_modes.push(engine.looper_mode_by_index(idx));
                                }
                                update_loopers(
                                    state,
                                    &current_looper_modes,
                                    true,
                                    &mut engine,
                                    &mut writer,
                                );
                            } else {
                                if update_loopers(
                                    state,
                                    &current_looper_modes,
                                    false,
                                    &mut engine,
                                    &mut writer,
                                ) {
                                    current_looper_modes.clear();
                                    for idx in 0..state.looper_count as usize {
                                        current_looper_modes.push(engine.looper_mode_by_index(idx));
                                    }
                                }
                            }
                            current_state = Some(state.clone());
                        }
                    }
                    Err(TryRecvError::Empty) => {
                        break;
                    }
                    Err(TryRecvError::Disconnected) => {
                        panic!("Midi Channel disconnected");
                    }
                }
            }

            jack::Control::Continue
        };
    let process = jack::ClosureProcessHandler::new(process_callback);

    // Activate the client, which starts the processing.
    let active_client = client.activate_async(Notifications, process).unwrap();

    thread::spawn(move || loop {
        match port_change_rx.recv() {
            Ok(ClientChange::AddPort(id)) => {
                let l = active_client
                    .as_client()
                    .register_port(&format!("loop{}_out_l", id), jack::AudioOut::default())
                    .map_err(|e| format!("could not create jack port: {:?}", e));
                let r = active_client
                    .as_client()
                    .register_port(&format!("loop{}_out_r", id), jack::AudioOut::default())
                    .map_err(|e| format!("could not create jack port: {:?}", e));

                match (l, r) {
                    (Ok(l), Ok(r)) => {
                        if let Err(_) =
                            port_change_resp_tx.send(ClientChangeResponse::PortAdded(id, l, r))
                        {
                            break;
                        }
                    }
                    (Err(e), _) | (_, Err(e)) => {
                        error!("Failed to register port with jack: {:?}", e);
                    }
                }
            }
            Ok(ClientChange::RemovePort(id, l, r)) => {
                if let Err(e) = active_client
                    .as_client()
                    .unregister_port(l)
                    .and_then(|()| active_client.as_client().unregister_port(r))
                {
                    error!("Unable to remove jack outputs: {:?}", e);
                }
                info!("removed ports for looper {}", id);
            }
            Ok(ClientChange::Shutdown) => {
                break;
            }
            Err(_) => {
                break;
            }
        }
    });

    // start the gui
    if let Some(gui) = gui {
        gui.start();
    } else {
        loop {
            let mut user_input = String::new();
            io::stdin().read_line(&mut user_input).ok();
            if user_input == "q" {
                break;
            }
        }
    }

    if let Err(_) = port_change_tx.send(ClientChange::Shutdown) {
        warn!("Failed to shutdown worker thread");
    }

    std::process::exit(0);
}
