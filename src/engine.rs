use jack::{AudioIn, Port, AudioOut, MidiIn};
use crossbeam_queue::SegQueue;
use std::sync::Arc;
use std::f32::NEG_INFINITY;
use crate::protos::*;
use crate::protos::PlayMode::Playing;

const SAMPLE_RATE: f64 = 44.100;

struct Looper {
    id: u32,
    buffers: Vec<[Vec<f32>; 2]>,
    play_mode: PlayMode,
    record_mode: RecordMode,
}

impl Looper {
    fn new(id: u32) -> Looper {
        let looper = Looper {
            id,
            buffers: vec![],
            play_mode: PlayMode::Paused,
            record_mode: RecordMode::None,
        };

        looper
    }

    fn set_record_mode(&mut self, mode: RecordMode) {
        self.record_mode = mode;
    }

    fn set_play_mode(&mut self, mode: PlayMode) {
        self.play_mode = mode;
    }
}

pub struct Engine {
    in_a: Port<AudioIn>,
    in_b: Port<AudioIn>,
    out_a: Port<AudioOut>,
    out_b: Port<AudioOut>,

    midi_in: Port<MidiIn>,

    gui_output: Arc<SegQueue<State>>,
    gui_input: Arc<SegQueue<Command>>,

    time: usize,

    loopers: Vec<Looper>,
    active: usize,

    id_counter: u32,
}

const THRESHOLD: f32 = 0.1;

fn max_abs(b: &[f32]) -> f32 {
    b.iter().map(|v| v.abs())
        .fold(NEG_INFINITY, |a, b| a.max(b))
}

impl Engine {
    pub fn new(in_a: Port<AudioIn>, in_b: Port<AudioIn>,
           out_a: Port<AudioOut>, out_b: Port<AudioOut>,
           midi_in: Port<MidiIn>,
           gui_output: Arc<SegQueue<State>>,
           gui_input: Arc<SegQueue<Command>>) -> Engine {
        Engine {
            in_a,
            in_b,
            out_a,
            out_b,
            midi_in,
            gui_output,
            gui_input,
            time: 0,
            loopers: vec![Looper::new(0)],
            active: 0,
            id_counter: 1,
        }
    }

    fn update_states(&mut self, ps: &jack::ProcessScope) {
        let midi_in = &self.midi_in;

        for e in midi_in.iter(ps) {
            println!("Got midi event {:?}", e);

            let time = &mut self.time;
            let looper = &mut self.loopers[self.active];
            let gui_output = &mut self.gui_output;

            if e.bytes.len() == 3 && e.bytes[0] == 144 {
                match e.bytes[1]{
                    60 => {
                        if looper.buffers.is_empty() || looper.play_mode == PlayMode::Paused {
                            looper.set_record_mode(RecordMode::Ready);
                        } else {
                            looper.set_record_mode(RecordMode::Overdub);
                            // todo: pre-allocate
                            looper.buffers.push([vec![], vec![]]);
                        }
                    }
                    62 => {
                        looper.set_record_mode(RecordMode::None);

                        if looper.play_mode == PlayMode::Paused {
                            looper.set_play_mode(PlayMode::Playing);
                        } else {
                            looper.set_play_mode(PlayMode::Paused);
                        }

                        *time = 0;
                    },
                    64 => {
                        // looper.set_play_mode(PlayMode::PAUSED);
                        self.loopers.push(Looper::new(self.id_counter));
                        self.id_counter += 1;
                        self.active = self.loopers.len() - 1;
                    }
                    _ => {}
                }
            }
        }
    }

    fn samples_to_time(time: usize) -> u64 {
        ((time as f64) / SAMPLE_RATE) as u64
    }

    pub fn process(&mut self, _ : &jack::Client, ps: &jack::ProcessScope) -> jack::Control {
        self.update_states(ps);

        let out_a_p = self.out_a.as_mut_slice(ps);
        let out_b_p = self.out_b.as_mut_slice(ps);
        let in_a_p = self.in_a.as_slice(ps);
        let in_b_p = self.in_b.as_slice(ps);

        let active = self.active;
        let looper = &mut self.loopers[active];

        let gui_output = &mut self.gui_output;

        if looper.record_mode == RecordMode::Ready && (max_abs(in_a_p) > THRESHOLD || max_abs(in_b_p) > THRESHOLD) {
            looper.buffers.clear();
            looper.set_record_mode(RecordMode::Record);
        }

        let mut l = in_a_p.to_vec();
        let mut r = in_b_p.to_vec();

        if looper.play_mode == PlayMode::Playing {
            let mut time = self.time;
            if !looper.buffers.is_empty() {
                for i in 0..l.len() {
                    for b in &looper.buffers {
                        l[i] += b[0][time % b[0].len()];
                        r[i] += b[1][time % b[1].len()];
                    }
                    time += 1;
                }
            }
        }

        out_a_p.clone_from_slice(&l);
        out_b_p.clone_from_slice(&r);

//        if looper.record_mode == RecordMode::Overdub && !looper.buffers.is_empty() {
//            let mut time = self.time;
//            if looper.buffers.len() > 1 {
//                looper.buffers.push([vec![], vec![]]);
//            } else {
//                panic!("entered overdub mode with too few buffers");
//            }
//        }

        if looper.record_mode == RecordMode::Record {
            if looper.buffers.is_empty() {
                looper.buffers.push([vec![], vec![]]);
            }
            looper.buffers[0][0].extend_from_slice(&l);
            looper.buffers[0][1].extend_from_slice(&r);
        }

        self.time += l.len();

        // TODO: make this non-allocating
        let time = self.time;
        let loop_states: Vec<LoopState> = self.loopers.iter().enumerate().map(|(i, l)| {
            let len = l.buffers.get(0).map(|l| l[0].len())
                .unwrap_or(0);

            let t = if len > 0 && l.play_mode == PlayMode::Playing {
                time % len
            } else {
                0
            };

            LoopState {
                id: l.id,
                record_mode: l.record_mode as i32,
                play_mode: l.play_mode as i32,
                time: Engine::samples_to_time(t) as i64,
                length: Engine::samples_to_time(len) as i64,
                active: i == active,
            }
        }).collect();

        gui_output.push(State{
            loops: loop_states,
        });

        jack::Control::Continue
    }
}