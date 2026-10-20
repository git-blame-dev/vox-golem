//! Opt-in, thread-local storage timings. Durations are inclusive and may overlap;
//! do not sum phases as an end-to-end duration. Counts include failed attempts.
//! Each instrumented call/span increments once, including nested same-phase calls.
//! `sqlite_read` includes query setup, each iterator step (including final None),
//! and bounded column extraction; JSON decoding happens outside these spans.
//! `sqlite_prepare` counts explicit prepare/cache lookups. Convenience query/execute
//! calls include their internal preparation in read/write instead. `refresh_reload`
//! counts reload attempts only (zero for a warm revision match).
//! Codec encode/decode spans cover individual serialization/JSON parsing calls,
//! not enclosing record hashing or validation; those remain uninstrumented residual.
//! Gateway native_request_decode/native_response_encode cover individual request
//! parses and response-envelope serialization. Owned reads exclude both from projection.
use serde::Serialize;
use std::cell::RefCell;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Metric {
    pub count: u64,
    pub total_ns: u64,
    pub max_ns: u64,
}

macro_rules! phases {
    ($($name:ident),+ $(,)?) => {
        #[derive(Clone, Copy)]
        #[allow(non_camel_case_types)]
        pub(crate) enum Phase { $($name),+ }
        #[derive(Clone, Debug, Default, Serialize)]
        pub struct Metrics { $(pub $name: Metric),+ }
        impl Metrics {
            fn get_mut(&mut self, phase: Phase) -> &mut Metric {
                match phase { $(Phase::$name => &mut self.$name),+ }
            }
        }
    };
}
phases!(
    sqlite_begin,
    sqlite_read,
    sqlite_write,
    sqlite_commit,
    sqlite_prepare,
    record_encode,
    record_decode,
    reducer,
    projection,
    refresh_reload,
    quota_check,
    native_request_decode,
    native_response_encode
);

#[derive(Clone, Debug, Serialize)]
pub struct Summary {
    pub schema: u8,
    pub metrics: Metrics,
}
thread_local! {
    static ACTIVE: RefCell<Option<Metrics>> = const { RefCell::new(None) };
}

struct Scope(Option<Metrics>);
impl Drop for Scope {
    fn drop(&mut self) {
        ACTIVE.with(|active| *active.borrow_mut() = self.0.take());
    }
}

/// Capture only this synchronous thread's action. Nested captures are isolated;
/// disabled nested captures suspend the outer collector. Panic restores it too.
pub fn capture<T>(enabled: bool, action: impl FnOnce() -> T) -> (T, Option<Summary>) {
    let previous = ACTIVE.with(|active| active.replace(enabled.then(Metrics::default)));
    let scope = Scope(previous);
    let value = action();
    let metrics = ACTIVE.with(|active| active.take());
    drop(scope);
    (value, metrics.map(|metrics| Summary { schema: 1, metrics }))
}

pub(crate) struct Span(Phase, Option<Instant>);
pub(crate) fn span(phase: Phase) -> Span {
    Span(
        phase,
        ACTIVE.with(|active| active.borrow().as_ref().map(|_| Instant::now())),
    )
}
impl Drop for Span {
    fn drop(&mut self) {
        if let Some(start) = self.1 {
            let ns = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
            ACTIVE.with(|active| {
                if let Some(metrics) = active.borrow_mut().as_mut() {
                    let metric = metrics.get_mut(self.0);
                    metric.count = metric.count.saturating_add(1);
                    metric.total_ns = metric.total_ns.saturating_add(ns);
                    metric.max_ns = metric.max_ns.max(ns);
                }
            });
        }
    }
}

pub(crate) fn measure<T>(phase: Phase, action: impl FnOnce() -> T) -> T {
    let _span = span(phase);
    action()
}

/// Each next() includes SQLite stepping and bounded column extraction, not decoding.
pub(crate) struct TimedRows<I>(I);
impl<I: Iterator> Iterator for TimedRows<I> {
    type Item = I::Item;
    fn next(&mut self) -> Option<Self::Item> {
        measure(Phase::sqlite_read, || self.0.next())
    }
}

use rusqlite::{
    CachedStatement, Connection, Params, Row, Statement, Transaction, TransactionBehavior,
};
pub(crate) trait ConnectionTiming {
    fn timed_execute(&self, sql: &str, params: impl Params) -> rusqlite::Result<usize>;
    fn timed_execute_batch(&self, sql: &str) -> rusqlite::Result<()>;
    fn timed_query_row<T>(
        &self,
        sql: &str,
        params: impl Params,
        f: impl FnOnce(&Row<'_>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T>;
    fn timed_prepare(&self, sql: &str) -> rusqlite::Result<Statement<'_>>;
    fn timed_prepare_cached(&self, sql: &str) -> rusqlite::Result<CachedStatement<'_>>;
    fn timed_transaction(&mut self) -> rusqlite::Result<Transaction<'_>>;
    fn timed_transaction_with_behavior(
        &mut self,
        behavior: TransactionBehavior,
    ) -> rusqlite::Result<Transaction<'_>>;
}
impl ConnectionTiming for Connection {
    fn timed_execute(&self, sql: &str, params: impl Params) -> rusqlite::Result<usize> {
        measure(Phase::sqlite_write, || self.execute(sql, params))
    }
    fn timed_execute_batch(&self, sql: &str) -> rusqlite::Result<()> {
        measure(Phase::sqlite_write, || self.execute_batch(sql))
    }
    fn timed_query_row<T>(
        &self,
        sql: &str,
        params: impl Params,
        f: impl FnOnce(&Row<'_>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T> {
        measure(Phase::sqlite_read, || self.query_row(sql, params, f))
    }
    fn timed_prepare(&self, sql: &str) -> rusqlite::Result<Statement<'_>> {
        measure(Phase::sqlite_prepare, || self.prepare(sql))
    }
    fn timed_prepare_cached(&self, sql: &str) -> rusqlite::Result<CachedStatement<'_>> {
        measure(Phase::sqlite_prepare, || self.prepare_cached(sql))
    }
    fn timed_transaction(&mut self) -> rusqlite::Result<Transaction<'_>> {
        measure(Phase::sqlite_begin, || self.transaction())
    }
    fn timed_transaction_with_behavior(
        &mut self,
        behavior: TransactionBehavior,
    ) -> rusqlite::Result<Transaction<'_>> {
        measure(Phase::sqlite_begin, || {
            self.transaction_with_behavior(behavior)
        })
    }
}
pub(crate) trait StatementTiming {
    fn timed_execute(&mut self, params: impl Params) -> rusqlite::Result<usize>;
    fn timed_query_map<T, F: FnMut(&Row<'_>) -> rusqlite::Result<T>>(
        &mut self,
        params: impl Params,
        f: F,
    ) -> rusqlite::Result<TimedRows<rusqlite::MappedRows<'_, F>>>;
}
impl StatementTiming for Statement<'_> {
    fn timed_execute(&mut self, params: impl Params) -> rusqlite::Result<usize> {
        measure(Phase::sqlite_write, || self.execute(params))
    }
    fn timed_query_map<T, F: FnMut(&Row<'_>) -> rusqlite::Result<T>>(
        &mut self,
        params: impl Params,
        f: F,
    ) -> rusqlite::Result<TimedRows<rusqlite::MappedRows<'_, F>>> {
        measure(Phase::sqlite_read, || self.query_map(params, f)).map(TimedRows)
    }
}
pub(crate) trait TransactionTiming {
    fn timed_commit(self) -> rusqlite::Result<()>;
}
impl TransactionTiming for Transaction<'_> {
    fn timed_commit(self) -> rusqlite::Result<()> {
        measure(Phase::sqlite_commit, || self.commit())
    }
}
