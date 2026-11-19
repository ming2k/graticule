//! Cost of the frame shapes the engine is built around.
//!
//! Run with `cargo bench`. Deliberately dependency-free: a `harness = false`
//! binary that measures and prints, so CI can run it without a benchmark
//! framework and the numbers cannot drift away from the code.
//!
//! The four cases are the claims worth defending:
//!
//! - **idle**: a clean damage tracker costs nothing and emits nothing;
//! - **one cell**: changing one cell touches one row;
//! - **streaming append**: a whole-viewport shift resolves to a scroll plus one
//!   new row — the byte count is what matters, and it is what a viewport repaint
//!   would have cost many times over;
//! - **all rows dirty**: what a conservative writer costs, the baseline the
//!   scroll search competes with.

#![allow(clippy::unwrap_used)]

use std::hint::black_box;
use std::time::{Duration, Instant};

use graticule_canvas::buffer::{BandRotation, CellGrid, Damage};
use graticule_canvas::render::diff::{self, DrawCmd};
use graticule_canvas::{Backend, Bce, Fit, Style, TerminalProfile};

const W: u16 = 200;
const H: u16 = 50;
const ITERS: u32 = 2_000;

/// A back/front pair plus its damage, the way a frame owns them.
struct Frame {
    back: CellGrid,
    front: CellGrid,
    damage: Damage,
}

impl Frame {
    fn new() -> Self {
        let mut back = CellGrid::new(W, H);
        let mut damage = Damage::new(H);
        // A full screen of text, as an application would have painted.
        for y in 0..H {
            let row = format!("row {y:>3} {}", "content ".repeat(20));
            back.put(&mut damage, 0, y, Fit::Clip, Style::RESET, &row);
        }
        // The front grid starts out showing exactly the same content, which is
        // what a settled frame looks like.
        let mut front = CellGrid::new(W, H);
        let mut front_damage = Damage::new(H);
        for y in 0..H {
            let row = format!("row {y:>3} {}", "content ".repeat(20));
            front.put(&mut front_damage, 0, y, Fit::Clip, Style::RESET, &row);
        }
        damage.clear();
        Self {
            back,
            front,
            damage,
        }
    }
}

/// Time `f`, returning the mean duration and the bytes emitted.
fn measure(mut f: impl FnMut() -> usize) -> (Duration, usize) {
    // Warm up so the measured runs see the same code layout as each other.
    let mut bytes = f();
    let start = Instant::now();
    for _ in 0..ITERS {
        bytes = black_box(f());
    }
    (start.elapsed() / ITERS, bytes)
}

fn emit(cmd: &DrawCmd) -> usize {
    let mut buf = Vec::with_capacity(64 * 1024);
    {
        let mut backend = Backend::new(&mut buf)
            .with_bce(Bce::Yes)
            .with_profile(TerminalProfile::direct_color());
        backend.render(cmd).unwrap();
    }
    buf.len()
}

fn report(label: &str, elapsed: Duration, bytes: usize) {
    println!("{label:<34} {:>10.2?}/frame   {bytes:>8} bytes", elapsed);
}

fn main() {
    // --- idle: nothing changed -------------------------------------------
    let mut frame = Frame::new();
    let (elapsed, bytes) = measure(|| {
        let cmd = diff::diff(&frame.back, &frame.damage, &frame.front);
        emit(&cmd)
    });
    report("idle frame", elapsed, bytes);

    // --- one cell ---------------------------------------------------------
    let mut glyph = 0u8;
    let (elapsed, bytes) = measure(|| {
        glyph = glyph.wrapping_add(1);
        frame.back.set(
            &mut frame.damage,
            100,
            25,
            graticule_canvas::Cell::narrow((b'A' + (glyph % 26)) as char, Style::RESET),
        );
        let cmd = diff::diff(&frame.back, &frame.damage, &frame.front);
        let bytes = emit(&cmd);
        diff::promote(&frame.back, &mut frame.damage, &mut frame.front);
        bytes
    });
    report("one cell changed", elapsed, bytes);

    // --- streaming append: shift history up, one new row ------------------
    let mut frame = Frame::new();
    let mut line = 0usize;
    let (elapsed, bytes) = measure(|| {
        // One line arrives at the bottom; every settled row above shifts up.
        line += 1;
        frame.back.rotate_band(0, H, BandRotation::Up, 1);
        let text = format!("new line {line}");
        frame
            .back
            .put(&mut frame.damage, 0, H - 1, Fit::Clip, Style::RESET, &text);
        frame.damage.mark_all();
        let cmd = diff::diff(&frame.back, &frame.damage, &frame.front);
        let bytes = emit(&cmd);
        diff::promote_scrolled(&frame.back, &mut frame.damage, &mut frame.front, &cmd);
        bytes
    });
    report("streaming append (scroll)", elapsed, bytes);

    // --- all rows dirty, one row changed ----------------------------------
    // What a conservative full-frame writer costs: the damage tracker says
    // "everything", the content says "one row", and the differ must walk the
    // whole dirty region to find out. This is the number the scroll search above
    // is competing with, in bytes and in time.
    let mut frame = Frame::new();
    let mut n = 0usize;
    let (elapsed, bytes) = measure(|| {
        n += 1;
        let text = format!("frame {n}");
        frame
            .back
            .put(&mut frame.damage, 0, 0, Fit::Clip, Style::RESET, &text);
        frame.damage.mark_all();
        let cmd = diff::diff(&frame.back, &frame.damage, &frame.front);
        let bytes = emit(&cmd);
        diff::promote(&frame.back, &mut frame.damage, &mut frame.front);
        bytes
    });
    report("all rows dirty, one changed", elapsed, bytes);
}
