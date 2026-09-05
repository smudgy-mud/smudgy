//! Exact allocation counts for SGR interpretation, with parameter construction excluded.
//! This executable always uses a counting allocator; its wall time is not a timing baseline.

use smudgy_bench::alloc::{CountingAllocator, per_call};
use smudgy_core::session::{connection::vt_processor::sgr_process, styled_line::Style};
use vtparse::CsiParam::{self, Integer as I, P};

#[global_allocator]
static ALLOC: CountingAllocator = CountingAllocator;

fn main() {
    let cases: &[(&str, &[CsiParam])] = &[
        ("empty_reset", &[]),
        ("reset", &[I(0)]),
        ("ansi_color", &[I(31)]),
        ("bold_color", &[I(1), P(b';'), I(31)]),
        ("palette", &[I(38), P(b';'), I(5), P(b';'), I(196)]),
        (
            "truecolor",
            &[
                I(38),
                P(b';'),
                I(2),
                P(b';'),
                I(1),
                P(b';'),
                I(2),
                P(b';'),
                I(3),
            ],
        ),
        (
            "colon_truecolor",
            &[
                I(38),
                P(b':'),
                I(2),
                P(b':'),
                P(b':'),
                I(1),
                P(b':'),
                I(2),
                P(b':'),
                I(3),
            ],
        ),
    ];
    for (name, params) in cases {
        let measure = per_call(10_000, || {
            std::hint::black_box(sgr_process(
                std::hint::black_box(Style::DEFAULT),
                std::hint::black_box(params),
            ));
        });
        println!(
            "{{\"case\":\"{name}\",\"allocations_per_call\":{},\"bytes_per_call\":{}}}",
            measure.count, measure.bytes
        );
    }
}
