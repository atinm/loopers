#[macro_use]
extern crate criterion;

use criterion::{BatchSize, Criterion};
use loopers_common::api::{FrameTime, LooperMode, Part, PartSet};
use loopers_common::gui_channel::GuiSender;
use loopers_engine::looper::Looper;

pub fn looper_benchmark(c: &mut Criterion) {
    let samples = [vec![0f32; 128], vec![0f32; 128]];

    c.bench_function("process input [128]", |b| {
        b.iter_batched(
            || Looper::new(1, PartSet::new(), GuiSender::disconnected()),
            |mut l| {
                l.process_input(0, &[&samples[0], &samples[1]], Part::A);
            },
            BatchSize::SmallInput,
        )
    });

    let mut o_l = vec![0f64; 128];
    let mut o_r = vec![0f64; 128];
    c.bench_function("process output [128]", |b| {
        b.iter_batched(
            || {
                let mut l = Looper::new(1, PartSet::new(), GuiSender::disconnected());
                l.transition_to(LooperMode::Recording, false);
                l.process_input(0, &[&samples[0], &samples[1]], Part::A);
                l.backend.as_mut().unwrap().process_until_done();
                l.transition_to(LooperMode::Playing, false);
                l.backend.as_mut().unwrap().process_until_done();
                l
            },
            |mut l| {
                l.process_output(FrameTime(128), &mut [&mut o_l, &mut o_r], Part::A, false);
            },
            BatchSize::SmallInput,
        )
    });

    let mut o_l = vec![0f64; 128];
    let mut o_r = vec![0f64; 128];
    c.bench_function("round trip [128]", |b| {
        b.iter_batched(
            || {
                let mut l = Looper::new(1, PartSet::new(), GuiSender::disconnected());
                l.transition_to(LooperMode::Recording, false);
                l.backend.as_mut().unwrap().process_until_done();
                l
            },
            |mut l| {
                l.process_input(0, &[&samples[0], &samples[1]], Part::A);
                l.backend.as_mut().unwrap().process_until_done();
                l.process_output(FrameTime(0), &mut [&mut o_l, &mut o_r], Part::A, false);
                l.backend.as_mut().unwrap().process_until_done();

                l.transition_to(LooperMode::Playing, false);
                l.backend.as_mut().unwrap().process_until_done();

                l.process_input(128, &[&samples[0], &samples[1]], Part::A);
                l.backend.as_mut().unwrap().process_until_done();
                l.process_output(FrameTime(128), &mut [&mut o_l, &mut o_r], Part::A, false);
                l.backend.as_mut().unwrap().process_until_done();
            },
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(looper_benchmarks, looper_benchmark);
criterion_main!(looper_benchmarks);
