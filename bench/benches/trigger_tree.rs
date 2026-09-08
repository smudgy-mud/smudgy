//! Inner triggers through the real engine: what a trigger with triggers inside it costs
//! per line, closed and open, and what a long watch costs.
//!
//! Groups:
//!   - `tree_gates/<outers>x<inner>/{closed,matching}`: many outers each holding a few
//!     inner triggers. `closed` feeds lines no outer matches, so the inner triggers are
//!     never scanned; the per-line cost is the outers' own patterns. `matching` feeds a
//!     line every outer matches, so every inner set is scanned too.
//!   - `tree_wide/<inner>`: one outer holding many inner triggers, matching every line.
//!   - `tree_deep/<levels>`: a chain nested to the depth limit, every level matching.
//!   - `tree_watching/<outers>`: outers with a live firing and inner triggers watching a
//!     range, over lines that match nothing: the cost of watching itself.
//!   - `tree_unlimited`: twenty outers watching with no limit through combat spam.
//!   - `tree_churn/{inner,top_level}`: registering and removing one trigger under a large
//!     outer versus at the top level, the rebuild each costs on the next line.
//!
//! Requires `smudgy_core`'s `bench-api` feature; the Cargo dev-dependency enables it.

use std::{hint::black_box, sync::Arc};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use smudgy_core::models::triggers::{InnerReach, LineReach, Overlap};
use smudgy_core::session::{
    runtime::{
        BenchActionQueue, IsolateId, Manager, Origin, PushTriggerParams, ScriptAction,
        SharedAutomationRegistry,
    },
    styled_line::StyledLine,
};

fn manager() -> (Manager, BenchActionQueue) {
    Manager::new_for_bench(
        Arc::new(";".to_string()),
        SharedAutomationRegistry::default(),
    )
}

fn push(mgr: &mut Manager, name: &str, pattern: &str, outer: Option<&str>, reach: InnerReach) {
    let name = Arc::new(name.to_string());
    let patterns = Arc::new(vec![pattern.to_string()]);
    let empty: Arc<Vec<String>> = Arc::new(Vec::new());
    mgr.push_trigger(PushTriggerParams {
        isolate: IsolateId::Main,
        origin: Origin::User,
        name: &name,
        patterns: &patterns,
        raw_patterns: &empty,
        anti_patterns: &empty,
        matchers: None,
        action: ScriptAction::Noop,
        prompt: false,
        enabled: true,
        priority: 0,
        fallthrough: true,
        fire_limit: None,
        line_limit: None,
        source: None,
        outer: outer.map(|o| Arc::new(o.to_string())),
        reach,
    })
    .expect("bench trigger registers");
}

fn line(text: &str) -> Arc<StyledLine> {
    Arc::new(StyledLine::new(text, Vec::new()))
}

fn feed(mgr: &mut Manager, queue: &BenchActionQueue, line: &Arc<StyledLine>) {
    mgr.process_incoming_line(black_box(line)).unwrap();
    queue.clear();
}

fn reach(within: LineReach) -> InnerReach {
    InnerReach {
        within_lines: within,
        ..InnerReach::DEFAULT
    }
}

fn bench_gates(c: &mut Criterion) {
    let mut group = c.benchmark_group("tree_gates");
    for (outers, inner) in [(200, 3), (2000, 3)] {
        let (mut mgr, queue) = manager();
        for o in 0..outers {
            let outer = format!("outer{o}");
            push(
                &mut mgr,
                &outer,
                &format!("^gate{o} "),
                None,
                InnerReach::DEFAULT,
            );
            for i in 0..inner {
                push(
                    &mut mgr,
                    &format!("{outer}/inner{i}"),
                    &format!("value{i}"),
                    Some(&outer),
                    InnerReach::DEFAULT,
                );
            }
        }
        // One line that opens nothing, one that opens every outer (every gate prefix
        // appears through a broad first token).
        let closed = line("The orc slashes you with a rusty sword.");
        feed(&mut mgr, &queue, &closed);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::new(format!("{outers}x{inner}"), "closed"),
            &closed,
            |b, l| b.iter(|| feed(&mut mgr, &queue, l)),
        );
        let matching = line(&format!(
            "{} value0 value1 value2",
            (0..outers).map(|o| format!("gate{o} ")).collect::<String>()
        ));
        group.bench_with_input(
            BenchmarkId::new(format!("{outers}x{inner}"), "matching"),
            &matching,
            |b, l| b.iter(|| feed(&mut mgr, &queue, l)),
        );
    }
    group.finish();
}

fn bench_wide(c: &mut Criterion) {
    let mut group = c.benchmark_group("tree_wide");
    for inner in [10_usize, 100, 1000] {
        let (mut mgr, queue) = manager();
        push(&mut mgr, "outer", "^You ", None, InnerReach::DEFAULT);
        for i in 0..inner {
            push(
                &mut mgr,
                &format!("inner{i}"),
                &format!("item{i:04}"),
                Some("outer"),
                InnerReach::DEFAULT,
            );
        }
        let l = line("You see item0007 and item0500 lying here.");
        feed(&mut mgr, &queue, &l);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(inner), &l, |b, l| {
            b.iter(|| feed(&mut mgr, &queue, l));
        });
    }
    group.finish();
}

fn bench_deep(c: &mut Criterion) {
    let mut group = c.benchmark_group("tree_deep");
    for levels in [2_usize, 8] {
        let (mut mgr, queue) = manager();
        push(&mut mgr, "level0", "a", None, InnerReach::DEFAULT);
        for level in 1..levels {
            push(
                &mut mgr,
                &format!("level{level}"),
                "a",
                Some(&format!("level{}", level - 1)),
                InnerReach::DEFAULT,
            );
        }
        let l = line("a line that matches every level");
        feed(&mut mgr, &queue, &l);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(levels), &l, |b, l| {
            b.iter(|| feed(&mut mgr, &queue, l));
        });
    }
    group.finish();
}

fn bench_watching(c: &mut Criterion) {
    let mut group = c.benchmark_group("tree_watching");
    for outers in [1_usize, 50] {
        let (mut mgr, queue) = manager();
        for o in 0..outers {
            let outer = format!("heading{o}");
            push(
                &mut mgr,
                &outer,
                &format!("^Heading {o}$"),
                None,
                InnerReach::DEFAULT,
            );
            push(
                &mut mgr,
                &format!("{outer}/item"),
                "^ (?<item>.+)$",
                Some(&outer),
                reach(LineReach::Lines(1_000_000)),
            );
        }
        for o in 0..outers {
            feed(&mut mgr, &queue, &line(&format!("Heading {o}")));
        }
        let l = line("The orc slashes you with a rusty sword.");
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(outers), &l, |b, l| {
            b.iter(|| feed(&mut mgr, &queue, l));
        });
    }
    group.finish();
}

fn bench_unlimited(c: &mut Criterion) {
    let (mut mgr, queue) = manager();
    for o in 0..20 {
        let outer = format!("blind{o}");
        push(
            &mut mgr,
            &outer,
            &format!("^Friend{o} is blinded!$"),
            None,
            InnerReach::DEFAULT,
        );
        push(
            &mut mgr,
            &format!("{outer}/sees"),
            &format!("^Friend{o} can see again$"),
            Some(&outer),
            InnerReach {
                overlap: Overlap::Each,
                ..reach(LineReach::Unlimited)
            },
        );
    }
    for o in 0..20 {
        feed(&mut mgr, &queue, &line(&format!("Friend{o} is blinded!")));
    }
    let spam: Vec<Arc<StyledLine>> = (0..1000)
        .map(|i| line(&format!("The orc slashes you for {} damage.", i % 37)))
        .collect();
    let mut group = c.benchmark_group("tree_unlimited");
    group.throughput(Throughput::Elements(spam.len() as u64));
    group.bench_function("20_outers_1000_lines", |b| {
        b.iter(|| {
            for l in &spam {
                feed(&mut mgr, &queue, l);
            }
        });
    });
    group.finish();
}

fn bench_churn(c: &mut Criterion) {
    let mut group = c.benchmark_group("tree_churn");
    let (mut mgr, queue) = manager();
    push(&mut mgr, "outer", "^You ", None, InnerReach::DEFAULT);
    for i in 0..1000 {
        push(
            &mut mgr,
            &format!("inner{i}"),
            &format!("item{i:04}"),
            Some("outer"),
            InnerReach::DEFAULT,
        );
    }
    for i in 0..1000 {
        push(
            &mut mgr,
            &format!("top{i}"),
            &format!("thing{i:04}"),
            None,
            InnerReach::DEFAULT,
        );
    }
    let quiet = line("nothing here matches");
    feed(&mut mgr, &queue, &quiet);
    group.bench_function("inner", |b| {
        b.iter(|| {
            push(
                &mut mgr,
                "churn",
                "churn",
                Some("outer"),
                InnerReach::DEFAULT,
            );
            feed(&mut mgr, &queue, &quiet);
            mgr.remove_trigger(&IsolateId::Main, &Origin::User, "churn");
            feed(&mut mgr, &queue, &quiet);
        });
    });
    group.bench_function("top_level", |b| {
        b.iter(|| {
            push(&mut mgr, "churn", "churn", None, InnerReach::DEFAULT);
            feed(&mut mgr, &queue, &quiet);
            mgr.remove_trigger(&IsolateId::Main, &Origin::User, "churn");
            feed(&mut mgr, &queue, &quiet);
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_gates,
    bench_wide,
    bench_deep,
    bench_watching,
    bench_unlimited,
    bench_churn
);
criterion_main!(benches);
