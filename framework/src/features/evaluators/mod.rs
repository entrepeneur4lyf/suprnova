//! Concrete [`Evaluator`](featureflag::evaluator::Evaluator)
//! implementations shipped by the framework. Each evaluator can be
//! used standalone, layered via
//! [`EvaluatorExt::chain`](featureflag::evaluator::EvaluatorExt::chain),
//! or wired in as the global default with
//! [`set_global_default`](featureflag::evaluator::set_global_default).

pub mod cached;
pub mod database;
