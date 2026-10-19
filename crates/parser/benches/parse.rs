//! Parser micro-benchmarks.
//!
//! The parser is the only CPU-bound code in the bot (everything else waits on
//! Postgres or Telegram). These benches aren't about chasing speed — a parse
//! is already sub-microsecond — they're a *regression guard*: the fuzzy
//! `strsim::levenshtein` keyword matching is the one place where adding aliases
//! or widening the adaptive threshold could quietly blow up the cost, so we
//! pin a number to each strategy branch.
//!
//! Fixed reference clock matches the unit tests (2026-05-08 12:00 Europe/Berlin)
//! so the inputs exercise the same code paths deterministically.

use chrono::{TimeZone, Utc};
use chrono_tz::Tz;
use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use parser::{parse, Language, ParseContext};

fn ctx() -> ParseContext {
    let tz: Tz = "Europe/Berlin".parse().unwrap();
    let now_local = tz.with_ymd_and_hms(2026, 5, 8, 12, 0, 0).unwrap();
    ParseContext {
        now_utc: now_local.with_timezone(&Utc),
        tz,
        language: Language::De,
    }
}

/// One representative input per parsing strategy, plus the failure path.
/// Names double as the criterion benchmark ids.
const CASES: &[(&str, &str)] = &[
    ("rel_compact", "5m Kaffee fertig"),
    ("rel_combined", "1Y2M15d8h40m20s test"),
    ("rel_longform", "30 minuten Pizza"),
    ("abs_date", "30.04.2027 14:30 Termin"),
    ("iso_date", "2026-12-31 sylvester"),
    ("bare_clock", "15:00 nap"),
    ("named_day", "do 14:00 Standup"),
    ("fuzzy_typo", "donnerstah 14:00 Standup"),
    ("recurring_weekly", "*mo,mi,fr 9 yoga"),
    ("recurring_relative", "*3M quartalsbericht"),
    ("no_time_expr", "Pizza essen"),
];

fn bench_parse(c: &mut Criterion) {
    let ctx = ctx();
    let mut group = c.benchmark_group("parse");
    for (name, input) in CASES {
        group.bench_with_input(BenchmarkId::from_parameter(name), input, |b, input| {
            b.iter(|| {
                // result may be Ok or Err depending on the case — black_box both
                // so the optimiser can't fold the call away.
                let _ = black_box(parse(black_box(input), black_box(&ctx)));
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_parse);
criterion_main!(benches);
