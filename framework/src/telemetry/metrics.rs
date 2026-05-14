//! Metrics facade — user-facing API for counters, histograms, and gauges.
//!
//! Stub implementation. Will be replaced with the full OnceLock-cached
//! implementation in Task 20.4.

/// Entry point for creating metric instruments.
pub struct Metrics;

impl Metrics {
    /// Get (or lazily create) a monotonically-increasing counter.
    pub fn counter(_name: &'static str) -> CounterHandle {
        CounterHandle
    }
    /// Get (or lazily create) a value distribution histogram.
    pub fn histogram(_name: &'static str) -> HistogramHandle {
        HistogramHandle
    }
    /// Get (or lazily create) a synchronous gauge.
    pub fn gauge(_name: &'static str) -> GaugeHandle {
        GaugeHandle
    }
}

/// Counter handle. Cloning is free.
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

/// Histogram handle.
#[derive(Clone)]
pub struct HistogramHandle;
impl HistogramHandle {
    #[inline(always)]
    pub fn record(&self, _value: f64) {}
    #[inline(always)]
    pub fn record_with(&self, _value: f64, _attrs: &[(&'static str, &str)]) {}
}

/// Gauge handle.
#[derive(Clone)]
pub struct GaugeHandle;
impl GaugeHandle {
    #[inline(always)]
    pub fn set(&self, _value: f64) {}
    #[inline(always)]
    pub fn set_with(&self, _value: f64, _attrs: &[(&'static str, &str)]) {}
}
