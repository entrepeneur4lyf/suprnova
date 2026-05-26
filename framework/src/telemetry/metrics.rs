//! Metrics facade — the user-facing API for counters, histograms, and gauges.
//!
//! Two compile-time shapes:
//!
//! - **`otel` enabled:** instruments are looked up in a global cache keyed by
//!   name, so `Metrics::counter("http.requests.total").inc()` is a constant-
//!   time hashmap lookup plus an atomic increment.
//! - **`otel` disabled (default):** every method is `#[inline(always)]` and
//!   discards its arguments. The compiler erases instrumentation entirely,
//!   so instrumented user code pays zero runtime cost in default builds.
//!
//! The public surface (`Metrics`, `CounterHandle`, `HistogramHandle`,
//! `GaugeHandle`) is identical in both modes.
//!
//! Naming: stable, ASCII, dot-delimited (e.g. `"http.requests.total"`,
//! `"http.request.duration"`). Standard OTel semantic conventions live in
//! `opentelemetry-semantic-conventions::metric::*`.

#[cfg(feature = "otel")]
mod real {
    use opentelemetry::KeyValue;
    use opentelemetry::global;
    use opentelemetry::metrics::{Counter, Gauge, Histogram};
    use std::sync::Arc;

    /// Build a fresh instrument handle on every call. We intentionally
    /// don't cache at this layer because OTel's SDK already caches
    /// instruments keyed by (name, unit, description) inside the meter,
    /// so repeated builds collapse to the same underlying `Arc<Counter>`
    /// / `Arc<Histogram>` / `Arc<Gauge>` after the first call. Caching
    /// at our layer was a trap: if `Metrics::counter("x")` ran once
    /// before `init_telemetry` installed the real provider, the cached
    /// handle was bound to the no-op meter permanently — silent data
    /// loss. Going through `global::meter("suprnova")` per call always
    /// resolves to whatever provider is currently installed.
    fn to_keyvalues(attrs: &[(&'static str, &str)]) -> Vec<KeyValue> {
        attrs
            .iter()
            .map(|(k, v)| KeyValue::new(*k, v.to_string()))
            .collect()
    }

    /// Entry point for creating metric instruments. Each call resolves
    /// the current provider via `global::meter("suprnova")` and asks
    /// it for an instrument with the given name. The OTel SDK caches
    /// instruments per `(name, unit, description)` internally.
    pub struct Metrics;

    impl Metrics {
        /// Get a monotonically-increasing counter.
        pub fn counter(name: &'static str) -> CounterHandle {
            let counter = global::meter("suprnova").u64_counter(name).build();
            CounterHandle(Arc::new(counter))
        }

        /// Get an `f64` histogram for value distributions.
        pub fn histogram(name: &'static str) -> HistogramHandle {
            let hist = global::meter("suprnova").f64_histogram(name).build();
            HistogramHandle(Arc::new(hist))
        }

        /// Get a synchronous `f64` gauge.
        pub fn gauge(name: &'static str) -> GaugeHandle {
            let gauge = global::meter("suprnova").f64_gauge(name).build();
            GaugeHandle(Arc::new(gauge))
        }
    }

    /// Cheap-to-clone handle backed by a cached `Counter<u64>`.
    #[derive(Clone)]
    pub struct CounterHandle(pub(crate) Arc<Counter<u64>>);

    impl CounterHandle {
        /// Increment by 1.
        pub fn inc(&self) {
            self.0.add(1, &[]);
        }
        /// Increment by `n`.
        pub fn inc_by(&self, n: u64) {
            self.0.add(n, &[]);
        }
        /// Increment by 1 with attributes.
        pub fn inc_with(&self, attrs: &[(&'static str, &str)]) {
            self.0.add(1, &to_keyvalues(attrs));
        }
    }

    /// Cheap-to-clone handle backed by a cached `Histogram<f64>`.
    #[derive(Clone)]
    pub struct HistogramHandle(pub(crate) Arc<Histogram<f64>>);

    impl HistogramHandle {
        /// Record a value.
        pub fn record(&self, value: f64) {
            self.0.record(value, &[]);
        }
        /// Record a value with attributes.
        pub fn record_with(&self, value: f64, attrs: &[(&'static str, &str)]) {
            self.0.record(value, &to_keyvalues(attrs));
        }
    }

    /// Cheap-to-clone handle backed by a cached `Gauge<f64>`.
    #[derive(Clone)]
    pub struct GaugeHandle(pub(crate) Arc<Gauge<f64>>);

    impl GaugeHandle {
        /// Set the gauge to `value`. (OTel's gauge API uses `record` —
        /// we expose `set` for clarity to users.)
        pub fn set(&self, value: f64) {
            self.0.record(value, &[]);
        }
        /// Set the gauge to `value` with attributes.
        pub fn set_with(&self, value: f64, attrs: &[(&'static str, &str)]) {
            self.0.record(value, &to_keyvalues(attrs));
        }
    }
}

#[cfg(not(feature = "otel"))]
mod stub {
    /// Entry point for creating metric instruments. In default builds
    /// (no `otel` feature) all methods are inert no-ops.
    pub struct Metrics;

    impl Metrics {
        #[inline(always)]
        pub fn counter(_name: &'static str) -> CounterHandle {
            CounterHandle
        }
        #[inline(always)]
        pub fn histogram(_name: &'static str) -> HistogramHandle {
            HistogramHandle
        }
        #[inline(always)]
        pub fn gauge(_name: &'static str) -> GaugeHandle {
            GaugeHandle
        }
    }

    /// Zero-cost stub. All methods compile to nothing.
    #[derive(Clone)]
    pub struct CounterHandle;

    impl CounterHandle {
        #[inline(always)]
        pub fn inc(&self) {}
        #[inline(always)]
        pub fn inc_by(&self, _n: u64) {}
        #[inline(always)]
        pub fn inc_with(&self, _attrs: &[(&'static str, &str)]) {}
    }

    /// Zero-cost stub. All methods compile to nothing.
    #[derive(Clone)]
    pub struct HistogramHandle;

    impl HistogramHandle {
        #[inline(always)]
        pub fn record(&self, _value: f64) {}
        #[inline(always)]
        pub fn record_with(&self, _value: f64, _attrs: &[(&'static str, &str)]) {}
    }

    /// Zero-cost stub. All methods compile to nothing.
    #[derive(Clone)]
    pub struct GaugeHandle;

    impl GaugeHandle {
        #[inline(always)]
        pub fn set(&self, _value: f64) {}
        #[inline(always)]
        pub fn set_with(&self, _value: f64, _attrs: &[(&'static str, &str)]) {}
    }
}

#[cfg(feature = "otel")]
pub use real::{CounterHandle, GaugeHandle, HistogramHandle, Metrics};
#[cfg(not(feature = "otel"))]
pub use stub::{CounterHandle, GaugeHandle, HistogramHandle, Metrics};

#[cfg(test)]
mod tests {
    use super::*;

    // These tests must compile and pass in BOTH feature configurations.
    // In the stub case they verify the API exists and accepts the right
    // shapes; in the real case they verify cache identity and
    // no-panic behavior before `init_telemetry` has installed providers.

    #[test]
    fn counter_noop_before_init() {
        let c = Metrics::counter("test.requests.total");
        c.inc();
        c.inc_by(42);
        c.inc_with(&[("env", "test"), ("route", "/")]);
    }

    #[test]
    fn histogram_noop_before_init() {
        let h = Metrics::histogram("test.request.duration");
        h.record(1.25);
        h.record_with(2.5, &[("route", "/api")]);
    }

    #[test]
    fn gauge_noop_before_init() {
        let g = Metrics::gauge("test.queue.depth");
        g.set(0.0);
        g.set_with(7.0, &[("queue", "default")]);
    }

    // Each Metrics::counter()/histogram()/gauge() call returns a fresh
    // handle (we don't cache at this layer — see the module-level note
    // for rationale). The OTel SDK is responsible for instrument
    // identity under the (name, unit, description) key. These tests
    // verify the API is idempotent in observable behavior: repeated
    // calls produce usable handles that record without panicking.
    #[cfg(feature = "otel")]
    #[test]
    fn counter_repeated_calls_produce_usable_handles() {
        let a = Metrics::counter("test.repeated.counter");
        let b = Metrics::counter("test.repeated.counter");
        a.inc();
        b.inc_by(5);
        a.inc_with(&[("env", "test")]);
    }

    #[cfg(feature = "otel")]
    #[test]
    fn histogram_repeated_calls_produce_usable_handles() {
        let a = Metrics::histogram("test.repeated.histogram");
        let b = Metrics::histogram("test.repeated.histogram");
        a.record(1.0);
        b.record_with(2.0, &[("env", "test")]);
    }

    #[cfg(feature = "otel")]
    #[test]
    fn gauge_repeated_calls_produce_usable_handles() {
        let a = Metrics::gauge("test.repeated.gauge");
        let b = Metrics::gauge("test.repeated.gauge");
        a.set(1.0);
        b.set_with(2.0, &[("env", "test")]);
    }
}
