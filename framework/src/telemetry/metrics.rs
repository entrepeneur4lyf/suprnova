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
    use opentelemetry::global;
    use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter};
    use opentelemetry::KeyValue;
    use std::collections::HashMap;
    use std::sync::{Arc, OnceLock, RwLock};

    /// Global meter handle. Created on first instrument lookup so that
    /// users can call `Metrics::counter(...)` even before
    /// `init_telemetry` runs — they just get a no-op meter until the
    /// global provider is installed.
    static METER: OnceLock<Meter> = OnceLock::new();
    static COUNTERS: OnceLock<RwLock<HashMap<&'static str, CounterHandle>>> = OnceLock::new();
    static HISTOGRAMS: OnceLock<RwLock<HashMap<&'static str, HistogramHandle>>> = OnceLock::new();
    static GAUGES: OnceLock<RwLock<HashMap<&'static str, GaugeHandle>>> = OnceLock::new();

    fn meter() -> &'static Meter {
        METER.get_or_init(|| global::meter("suprnova"))
    }

    fn counters() -> &'static RwLock<HashMap<&'static str, CounterHandle>> {
        COUNTERS.get_or_init(|| RwLock::new(HashMap::new()))
    }
    fn histograms() -> &'static RwLock<HashMap<&'static str, HistogramHandle>> {
        HISTOGRAMS.get_or_init(|| RwLock::new(HashMap::new()))
    }
    fn gauges() -> &'static RwLock<HashMap<&'static str, GaugeHandle>> {
        GAUGES.get_or_init(|| RwLock::new(HashMap::new()))
    }

    fn to_keyvalues(attrs: &[(&'static str, &str)]) -> Vec<KeyValue> {
        attrs
            .iter()
            .map(|(k, v)| KeyValue::new(*k, v.to_string()))
            .collect()
    }

    /// Entry point for creating metric instruments. Instruments are
    /// cached by name; repeated calls with the same name return a handle
    /// backed by the same instrument.
    pub struct Metrics;

    impl Metrics {
        /// Get (or lazily create) a monotonically-increasing counter.
        pub fn counter(name: &'static str) -> CounterHandle {
            let map = counters();
            if let Some(h) = map.read().unwrap().get(name) {
                return h.clone();
            }
            let mut w = map.write().unwrap();
            // Double-check after acquiring write lock.
            if let Some(h) = w.get(name) {
                return h.clone();
            }
            let counter = meter().u64_counter(name).build();
            let handle = CounterHandle(Arc::new(counter));
            w.insert(name, handle.clone());
            handle
        }

        /// Get (or lazily create) an `f64` histogram for value distributions.
        pub fn histogram(name: &'static str) -> HistogramHandle {
            let map = histograms();
            if let Some(h) = map.read().unwrap().get(name) {
                return h.clone();
            }
            let mut w = map.write().unwrap();
            if let Some(h) = w.get(name) {
                return h.clone();
            }
            let hist = meter().f64_histogram(name).build();
            let handle = HistogramHandle(Arc::new(hist));
            w.insert(name, handle.clone());
            handle
        }

        /// Get (or lazily create) a synchronous `f64` gauge.
        pub fn gauge(name: &'static str) -> GaugeHandle {
            let map = gauges();
            if let Some(h) = map.read().unwrap().get(name) {
                return h.clone();
            }
            let mut w = map.write().unwrap();
            if let Some(h) = w.get(name) {
                return h.clone();
            }
            let gauge = meter().f64_gauge(name).build();
            let handle = GaugeHandle(Arc::new(gauge));
            w.insert(name, handle.clone());
            handle
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

    #[cfg(feature = "otel")]
    #[test]
    fn counter_same_name_returns_cached_handle() {
        let a = Metrics::counter("test.cache.counter");
        let b = Metrics::counter("test.cache.counter");
        // Both handles must wrap the same underlying counter instrument.
        assert!(std::sync::Arc::ptr_eq(&a.0, &b.0));
    }

    #[cfg(feature = "otel")]
    #[test]
    fn histogram_same_name_returns_cached_handle() {
        let a = Metrics::histogram("test.cache.histogram");
        let b = Metrics::histogram("test.cache.histogram");
        assert!(std::sync::Arc::ptr_eq(&a.0, &b.0));
    }

    #[cfg(feature = "otel")]
    #[test]
    fn gauge_same_name_returns_cached_handle() {
        let a = Metrics::gauge("test.cache.gauge");
        let b = Metrics::gauge("test.cache.gauge");
        assert!(std::sync::Arc::ptr_eq(&a.0, &b.0));
    }
}
